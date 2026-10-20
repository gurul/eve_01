//! Owner identity: tag each detected face `owner` or `unknown`.
//!
//! The robot should know whose desk it sits on. While the listen key is held
//! (Option = dictation, so the owner is at the laptop, facing the robot) the
//! daemon enrols the face it sees; afterwards every face cmd carries
//! `who:"owner"` when the print of the largest face is close to one of the
//! enrolled prints, `who:"unknown"` otherwise.
//!
//! Two halves, kept apart so the policy and the store are testable without a
//! Mac (same shape as eve-vision and listen_key):
//!
//! * Pure core — [`OwnerIdentity`] holds up to [`CAPACITY`] owner prints as
//!   plain vectors of floats, classifies by minimum distance against them,
//!   persists to JSON, and owns the enrolment policy ([`EnrolmentGate`]).
//!   [`FaceIdentity`] is the per-frame driver: print, classify, maybe enrol,
//!   log transitions. Everything it touches is injectable.
//! * Platform adapter, darwin — [`make_describer`] wraps Vision's
//!   `VNGenerateImageFeaturePrintRequest` (revision 2) over a CGImage
//!   cropped to the face rect padded by 25 %.
//!
//! Storage choice: Vision's `computeDistance:toFeaturePrintObservation:` is
//! plain Euclidean distance over the observation's raw float32 `data()`
//! (measured identical to 1e-8 on macOS 26.5, revision 1 and 2). So the
//! adapter unpacks the print into a vector of floats once and the core does
//! the arithmetic itself. No observation objects are kept and nothing is
//! rebuilt on load; the JSON file holds the vectors verbatim.
//!
//! Revision 2 (macOS 14+) prints are 768 unit-norm floats, so distances lie in
//! 0..2. [`DEFAULT_THRESHOLD`] is a starting point, not a measurement — tune it
//! on the bench with `EVE_OWNER_THRESHOLD` and the `identity: ... d=` log
//! lines.
//!
//! Thread model: [`FaceIdentity::process`] runs on eve-vision's single worker
//! thread, right after the detect for the same frame, so Vision requests never
//! overlap. The core takes a lock around every read and write, because
//! `reset` (IPC) and `status` come from the runtime's threads.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eve_hooks::HookError;
use eve_vision::{largest_rect, Clock, Frame, Rect, ValueError};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

#[cfg(target_os = "macos")]
mod mac;

#[cfg(test)]
mod tests;

pub const WHO_OWNER: &str = "owner";
pub const WHO_UNKNOWN: &str = "unknown";

/// How many owner prints we keep. More prints = more poses/lighting covered.
pub const CAPACITY: usize = 24;
/// Min distance to any enrolled print below which a face is the owner.
pub const DEFAULT_THRESHOLD: f64 = 0.9;
/// Enrolment policy: only with the listen key down, exactly one face in the
/// frame, the face at least this wide (as % of frame width), and at most one
/// enrolment per interval so a single 3 s hold does not fill the store with
/// near-identical prints.
pub const MIN_FACE_SIZE_PCT: f64 = 20.0;
pub const ENROL_INTERVAL_SECS: f64 = 2.0;
/// Transition log lines ("owner recognised" / "unknown face") at most this often.
pub const TRANSITION_LOG_SECS: f64 = 30.0;
/// Feature print revision we request (VNGenerateImageFeaturePrintRequestRevision2).
pub const FEATURE_PRINT_REVISION: i64 = 2;
/// Grow the face rect by this fraction of its size on every side before printing.
pub const CROP_PAD: f64 = 0.25;
/// Persisted file format version.
pub const FILE_VERSION: i64 = 1;

/// One feature print: Python's `list[float]`.
pub type Vector = Vec<f64>;
/// Distance between two prints. An `Err` is the exception Python's distance
/// raises (a length mismatch for [`euclidean`]); it propagates like one.
pub type Distance = Arc<dyn Fn(&[f64], &[f64]) -> anyhow::Result<f64> + Send + Sync>;
/// Worker thread. Feature print of one face; `Ok(None)` when Vision could not
/// produce one (bad crop, decode failure); `Err` is Python's exception.
pub type Describer = Arc<dyn Fn(&Frame, &Rect) -> anyhow::Result<Option<Vector>> + Send + Sync>;
/// Is the listen key down right now? Polled once per frame.
pub type ListenDown = Arc<dyn Fn() -> bool + Send + Sync>;

fn env_var(key: &str) -> Option<String> {
    std::env::var_os(key).map(|v| v.to_string_lossy().into_owned())
}

/// `~/.config/eve-bridge/owner_faceprints.json`, or `EVE_OWNER_PRINTS`
/// (user-expanded), or under `XDG_CONFIG_HOME` when that is set.
pub fn default_path() -> PathBuf {
    default_path_from(env_var("EVE_OWNER_PRINTS").as_deref(), env_var("XDG_CONFIG_HOME").as_deref())
}

/// [`default_path`] with the two environment values passed in.
pub fn default_path_from(override_path: Option<&str>, xdg_config_home: Option<&str>) -> PathBuf {
    if let Some(o) = override_path.filter(|s| !s.is_empty()) {
        return eve_vision::expanduser(o);
    }
    let root = match xdg_config_home.filter(|s| !s.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => eve_vision::expanduser("~").join(".config"),
    };
    root.join("eve-bridge").join("owner_faceprints.json")
}

/// `EVE_OWNER_THRESHOLD` as a positive finite float, else [`DEFAULT_THRESHOLD`]
/// (with a warning when the value was set but unusable).
pub fn configured_threshold() -> f64 {
    configured_threshold_from(env_var("EVE_OWNER_THRESHOLD").as_deref())
}

/// [`configured_threshold`] with the environment value passed in.
pub fn configured_threshold_from(raw: Option<&str>) -> f64 {
    let raw = eve_hooks::py_strip(raw.unwrap_or(""));
    if raw.is_empty() {
        return DEFAULT_THRESHOLD;
    }
    let Some(value) = py_float_str(raw) else {
        tracing::warn!(
            "identity: bad EVE_OWNER_THRESHOLD={}; using {:.2}",
            eve_ipc::py_str_repr(raw),
            DEFAULT_THRESHOLD
        );
        return DEFAULT_THRESHOLD;
    };
    if !value.is_finite() || value <= 0.0 {
        tracing::warn!(
            "identity: EVE_OWNER_THRESHOLD={} out of range; using {:.2}",
            eve_ipc::py_str_repr(raw),
            DEFAULT_THRESHOLD
        );
        return DEFAULT_THRESHOLD;
    }
    value
}

/// Python's `float(str)`: surrounding whitespace stripped, `_` allowed only
/// between digits, `inf`/`infinity`/`nan` in any case with a sign.
pub fn py_float_str(s: &str) -> Option<f64> {
    let s = eve_hooks::py_strip(s);
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'_' {
            let before = i > 0 && b[i - 1].is_ascii_digit();
            let after = b.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !before || !after {
                return None;
            }
        }
    }
    let cleaned: String = s.chars().filter(|&c| c != '_').collect();
    // Rust's parser takes the same grammar for the rest (sign, digits, one
    // dot, exponent, inf/infinity/nan); anything with other characters fails.
    if !cleaned.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// CPython 3.12+'s `sum()` over floats: Neumaier-compensated, with the
/// compensation added at the end only when it is finite and non-zero.
fn py_sum(items: impl Iterator<Item = f64>) -> f64 {
    let mut f_result = 0.0f64;
    let mut c = 0.0f64;
    for x in items {
        let t = f_result + x;
        if f_result.abs() >= x.abs() {
            c += (f_result - t) + x;
        } else {
            c += (x - t) + f_result;
        }
        f_result = t;
    }
    if c != 0.0 && c.is_finite() {
        f_result += c;
    }
    f_result
}

/// What Vision's computeDistance does over the raw print floats.
pub fn euclidean(a: &[f64], b: &[f64]) -> Result<f64, ValueError> {
    if a.len() != b.len() {
        return Err(ValueError(format!("print length mismatch: {} vs {}", a.len(), b.len())));
    }
    Ok(py_sum(a.iter().zip(b).map(|(x, y)| (x - y) * (x - y))).sqrt())
}

fn default_distance() -> Distance {
    Arc::new(|a, b| Ok(euclidean(a, b)?))
}

/// Python's `time.time()`.
fn wall_clock() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// An OSError the way Python prints one for a call on `path`:
/// `[Errno N] strerror: 'path'`.
fn os_error(e: io::Error, path: &Path) -> io::Error {
    let text = format!(
        "{}: {}",
        eve_ipc::py_os_error_str(&e),
        eve_ipc::py_str_repr(&path.to_string_lossy())
    );
    io::Error::new(e.kind(), text)
}

// ---- pure core --------------------------------------------------------------

struct Store {
    prints: Vec<Vector>,
    /// wall clock of the last enrolment
    enrolled_at: Option<f64>,
}

/// The owner's prints, classification, and the JSON store. Thread-safe.
pub struct OwnerIdentity {
    pub distance: Distance,
    pub threshold: f64,
    pub capacity: usize,
    pub path: Option<PathBuf>,
    store: Mutex<Store>,
}

impl Default for OwnerIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl OwnerIdentity {
    /// `OwnerIdentity()`: Euclidean distance, [`DEFAULT_THRESHOLD`],
    /// [`CAPACITY`], no path. The `with_*` methods are Python's keywords.
    pub fn new() -> Self {
        OwnerIdentity {
            distance: default_distance(),
            threshold: DEFAULT_THRESHOLD,
            capacity: CAPACITY,
            path: None,
            store: Mutex::new(Store { prints: Vec::new(), enrolled_at: None }),
        }
    }

    pub fn with_distance(mut self, distance: Distance) -> Self {
        self.distance = distance;
        self
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// `len(core)`.
    pub fn len(&self) -> usize {
        self.store.lock().prints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A copy of the held prints (Python's `core.prints`).
    pub fn prints(&self) -> Vec<Vector> {
        self.store.lock().prints.clone()
    }

    /// Wall clock of the last enrolment, None when nothing was enrolled.
    pub fn enrolled_at(&self) -> Option<f64> {
        self.store.lock().enrolled_at
    }

    /// ("owner"|"unknown", min distance). Distance is None with no prints.
    pub fn classify(&self, vec: &[f64]) -> anyhow::Result<(&'static str, Option<f64>)> {
        let prints = self.store.lock().prints.clone();
        if prints.is_empty() {
            return Ok((WHO_UNKNOWN, None));
        }
        // Python's min(): the first value no later one is strictly below.
        let mut d = (self.distance)(vec, &prints[0])?;
        for p in &prints[1..] {
            let x = (self.distance)(vec, p)?;
            if x < d {
                d = x;
            }
        }
        Ok((if d < self.threshold { WHO_OWNER } else { WHO_UNKNOWN }, Some(d)))
    }

    /// Add a print. When full, replace the enrolled print nearest to the new
    /// one, so the store keeps its spread over poses instead of drifting to
    /// one look. Returns the 1-based slot the print went into. `now` is the
    /// wall clock to record (default `time.time()`).
    pub fn enrol(&self, vec: &[f64], now: Option<f64>) -> anyhow::Result<usize> {
        let mut st = self.store.lock();
        let slot = if st.prints.len() < self.capacity {
            st.prints.push(vec.to_vec());
            st.prints.len()
        } else {
            if st.prints.is_empty() {
                anyhow::bail!("min() iterable argument is empty");
            }
            let mut idx = 0;
            let mut best = (self.distance)(vec, &st.prints[0])?;
            for i in 1..st.prints.len() {
                let d = (self.distance)(vec, &st.prints[i])?;
                if d < best {
                    best = d;
                    idx = i;
                }
            }
            st.prints[idx] = vec.to_vec();
            idx + 1
        };
        st.enrolled_at = Some(now.unwrap_or_else(wall_clock));
        Ok(slot)
    }

    /// Forget every print, in memory and on disk. Returns how many were held.
    /// A missing file is fine; any other unlink error is returned (memory is
    /// already cleared), as Python lets it raise.
    pub fn reset(&self) -> io::Result<usize> {
        let n = {
            let mut st = self.store.lock();
            let n = st.prints.len();
            st.prints = Vec::new();
            st.enrolled_at = None;
            n
        };
        if let Some(path) = &self.path {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(os_error(e, path)),
            }
        }
        Ok(n)
    }

    // ---- persistence ----

    /// The stored document: `version`, `revision`, `threshold`, `enrolled_at`,
    /// `prints`, in that order.
    pub fn snapshot(&self) -> Value {
        let st = self.store.lock();
        json!({
            "version": FILE_VERSION,
            "revision": FEATURE_PRINT_REVISION,
            "threshold": self.threshold,
            "enrolled_at": st.enrolled_at,
            "prints": st.prints,
        })
    }

    /// The snapshot as `json.dump(..., separators=(",", ":"))` writes it:
    /// compact, floats as Python's repr, NaN/Infinity as Python allows them.
    fn snapshot_text(&self) -> String {
        fn num(f: f64) -> String {
            if f.is_nan() {
                "NaN".to_string()
            } else if f.is_infinite() {
                if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
            } else {
                eve_ipc::py_float_repr(f)
            }
        }
        let st = self.store.lock();
        let enrolled = st.enrolled_at.map(num).unwrap_or_else(|| "null".to_string());
        let prints = st
            .prints
            .iter()
            .map(|p| format!("[{}]", p.iter().map(|&x| num(x)).collect::<Vec<_>>().join(",")))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"version\":{FILE_VERSION},\"revision\":{FEATURE_PRINT_REVISION},\"threshold\":{},\"enrolled_at\":{enrolled},\"prints\":[{prints}]}}",
            num(self.threshold)
        )
    }

    /// Write the store as JSON, mode 600, via a temp file + rename. `path`
    /// overrides the core's own path.
    pub fn save(&self, path: Option<&Path>) -> io::Result<PathBuf> {
        let target = match path {
            Some(p) => p.to_path_buf(),
            None => match &self.path {
                Some(p) => p.clone(),
                None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "no path to save to")),
            },
        };
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| os_error(e, parent))?;
        }
        let mut name = target.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        let tmp = target.with_file_name(name);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| os_error(e, &tmp))?;
        let text = self.snapshot_text();
        f.write_all(text.as_bytes())
            .and_then(|_| f.flush())
            .map_err(|e| io::Error::new(e.kind(), eve_ipc::py_os_error_str(&e)))?;
        drop(f);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).map_err(|e| os_error(e, &tmp))?;
        fs::rename(&tmp, &target).map_err(|e| {
            let text = format!(
                "{}: {} -> {}",
                eve_ipc::py_os_error_str(&e),
                eve_ipc::py_str_repr(&tmp.to_string_lossy()),
                eve_ipc::py_str_repr(&target.to_string_lossy())
            );
            io::Error::new(e.kind(), text)
        })?;
        Ok(target)
    }

    /// Read the store. Missing file = empty store. A malformed file is
    /// logged and ignored (never fails, and keeps what is in memory).
    /// Returns the number of prints. `path` overrides the core's own path.
    pub fn load(&self, path: Option<&Path>) -> usize {
        let source = match path {
            Some(p) => p.to_path_buf(),
            None => match &self.path {
                Some(p) => p.clone(),
                None => return 0,
            },
        };
        let (raw, prints) = match read_store(&source) {
            Ok(Some(v)) => v,
            Ok(None) => return 0,
            Err(msg) => {
                tracing::warn!("identity: ignoring {}: {}", source.display(), msg);
                return 0;
            }
        };
        let enrolled_at = match raw.get("enrolled_at") {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        };
        let mut st = self.store.lock();
        st.prints = prints.into_iter().take(self.capacity).collect();
        st.enrolled_at = enrolled_at;
        st.prints.len()
    }
}

/// The file's JSON and its validated prints; `Ok(None)` when the file does
/// not exist; `Err` with the text of the exception Python would log.
fn read_store(source: &Path) -> Result<Option<(Value, Vec<Vector>)>, String> {
    let bytes = match fs::read(source) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(os_error(e, source).to_string()),
    };
    let text = String::from_utf8(bytes).map_err(|e| utf8_error_text(e.as_bytes(), e.utf8_error()))?;
    let raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let prints = parse_prints(&raw)?;
    Ok(Some((raw, prints)))
}

/// Python's UnicodeDecodeError text for `read_text(encoding="utf-8")`.
fn utf8_error_text(bytes: &[u8], e: std::str::Utf8Error) -> String {
    let pos = e.valid_up_to();
    match e.error_len() {
        None if bytes.len() - pos == 1 => {
            format!("'utf-8' codec can't decode byte 0x{:02x} in position {pos}: unexpected end of data", bytes[pos])
        }
        None => format!(
            "'utf-8' codec can't decode bytes in position {}-{}: unexpected end of data",
            pos,
            bytes.len() - 1
        ),
        Some(_) => {
            let b = bytes[pos];
            let reason = if (0xC2..=0xF4).contains(&b) { "invalid continuation byte" } else { "invalid start byte" };
            format!("'utf-8' codec can't decode byte 0x{b:02x} in position {pos}: {reason}")
        }
    }
}

/// `float(x)` for a value `json.loads` produced; Err is the exception text.
fn py_float_value(x: &Value) -> Result<f64, String> {
    match x {
        Value::Number(n) => n.as_f64().ok_or_else(|| "int too large to convert to float".to_string()),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => py_float_str(s)
            .ok_or_else(|| format!("could not convert string to float: {}", eve_ipc::py_str_repr(s))),
        other => Err(format!(
            "float() argument must be a string or a real number, not {}",
            eve_ipc::py_str_repr(eve_hooks::py_type_name(other))
        )),
    }
}

/// `x == n` for a JSON value against a Python int (`1 == 1.0 == True`).
fn py_eq_int(x: Option<&Value>, n: i64) -> bool {
    match x {
        Some(Value::Number(v)) => match (v.as_i64(), v.as_f64()) {
            (Some(i), _) => i == n,
            (None, Some(f)) => f == n as f64,
            _ => false,
        },
        Some(Value::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

fn repr_opt(x: Option<&Value>) -> String {
    eve_ipc::py_repr(x.unwrap_or(&Value::Null))
}

fn parse_prints(raw: &Value) -> Result<Vec<Vector>, String> {
    let Value::Object(obj) = raw else {
        return Err("not a JSON object".to_string());
    };
    if !py_eq_int(obj.get("version"), FILE_VERSION) {
        return Err(format!("unsupported version {}", repr_opt(obj.get("version"))));
    }
    if !py_eq_int(obj.get("revision"), FEATURE_PRINT_REVISION) {
        return Err(format!(
            "prints are revision {}, want {} — reset",
            repr_opt(obj.get("revision")),
            FEATURE_PRINT_REVISION
        ));
    }
    let Some(Value::Array(prints)) = obj.get("prints") else {
        return Err("prints is not a list".to_string());
    };
    let mut out: Vec<Vector> = Vec::with_capacity(prints.len());
    for p in prints {
        let items = match p {
            Value::Array(items) if !items.is_empty() => items,
            _ => return Err("print is not a non-empty list".to_string()),
        };
        let vec = items.iter().map(py_float_value).collect::<Result<Vector, String>>()?;
        if vec.iter().any(|x| !x.is_finite()) {
            return Err("print holds a non-finite value".to_string());
        }
        out.push(vec);
    }
    if out.iter().any(|p| p.len() != out[0].len()) {
        return Err("prints differ in length".to_string());
    }
    Ok(out)
}

/// When may this face be enrolled? Listen key down, exactly one face,
/// face wide enough, and the interval since the last enrolment elapsed.
#[derive(Debug, Clone)]
pub struct EnrolmentGate {
    pub min_size_pct: f64,
    pub interval: f64,
    last: f64,
}

impl Default for EnrolmentGate {
    fn default() -> Self {
        Self::new(MIN_FACE_SIZE_PCT, ENROL_INTERVAL_SECS)
    }
}

impl EnrolmentGate {
    pub fn new(min_size_pct: f64, interval: f64) -> Self {
        EnrolmentGate { min_size_pct, interval, last: f64::NEG_INFINITY }
    }

    pub fn allows(&self, listen_down: bool, n_faces: usize, size_pct: f64, now: f64) -> bool {
        if !listen_down || n_faces != 1 || size_pct < self.min_size_pct {
            return false;
        }
        now - self.last >= self.interval
    }

    pub fn mark(&mut self, now: f64) {
        self.last = now;
    }
}

struct DriverState {
    gate: EnrolmentGate,
    enrolments: u64,
    /// last classified face
    last_who: Option<String>,
    last_distance: Option<f64>,
    transition_logged_at: f64,
    save_failed: bool,
}

/// Per-frame driver. [`FaceIdentity::process`] runs on the vision worker thread.
///
/// `listen_down` is polled per frame (the daemon owns the key state);
/// `describe` turns the face crop into a print, or None.
pub struct FaceIdentity {
    pub core: Arc<OwnerIdentity>,
    describe: Describer,
    listen_down: ListenDown,
    clock: Clock,
    state: Mutex<DriverState>,
}

impl FaceIdentity {
    /// `FaceIdentity(core, describe)`: listen key never down, default gate,
    /// monotonic clock. The `with_*` methods are Python's keywords.
    pub fn new(core: Arc<OwnerIdentity>, describe: Describer) -> Self {
        FaceIdentity {
            core,
            describe,
            listen_down: Arc::new(|| false),
            clock: Arc::new(eve_vision::monotonic),
            state: Mutex::new(DriverState {
                gate: EnrolmentGate::default(),
                enrolments: 0,
                last_who: None,
                last_distance: None,
                transition_logged_at: f64::NEG_INFINITY,
                save_failed: false,
            }),
        }
    }

    pub fn with_listen_down(mut self, listen_down: ListenDown) -> Self {
        self.listen_down = listen_down;
        self
    }

    pub fn with_gate(self, gate: EnrolmentGate) -> Self {
        self.state.lock().gate = gate;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Enrolments made by this driver (this run).
    pub fn enrolments(&self) -> u64 {
        self.state.lock().enrolments
    }

    /// The last classified face, None before the first.
    pub fn last_who(&self) -> Option<String> {
        self.state.lock().last_who.clone()
    }

    /// The daemon clears this on reset (`identity.last_who = None`).
    pub fn set_last_who(&self, who: Option<String>) {
        self.state.lock().last_who = who;
    }

    pub fn last_distance(&self) -> Option<f64> {
        self.state.lock().last_distance
    }

    /// Worker thread. Classify the largest face, enrol per policy. A describer
    /// failure is logged and answers "unknown"; an `Err` is only a distance
    /// failure, which Python lets propagate to the tracker.
    pub fn process(&self, frame: &Frame, rects: &[Rect]) -> anyhow::Result<String> {
        if rects.is_empty() {
            return Ok(WHO_UNKNOWN.to_string());
        }
        let face = largest_rect(rects);
        let described = catch_unwind(AssertUnwindSafe(|| (self.describe)(frame, &face)))
            .unwrap_or_else(|p| Err(anyhow::anyhow!("panic: {}", panic_text(p.as_ref()))));
        let vec = match described {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(WHO_UNKNOWN.to_string()),
            Err(e) => {
                tracing::error!("identity: feature print failed on frame {}: {:#}", frame.seq, e);
                return Ok(WHO_UNKNOWN.to_string());
            }
        };
        let now = (self.clock)();
        let size_pct = if frame.w > 0 { face.w / frame.w as f64 * 100.0 } else { 0.0 };
        let listen_down = (self.listen_down)();
        let allowed = {
            let mut st = self.state.lock();
            let ok = st.gate.allows(listen_down, rects.len(), size_pct, now);
            if ok {
                st.gate.mark(now);
            }
            ok
        };
        if allowed {
            let slot = self.core.enrol(&vec, None)?;
            self.state.lock().enrolments += 1;
            tracing::info!(
                "identity: enrolled owner print #{} (size={:.0}%, {}/{} held)",
                slot,
                size_pct,
                self.core.len(),
                self.core.capacity
            );
            self.persist();
        }
        let (who, d) = self.core.classify(&vec)?;
        self.note(who, d, now);
        Ok(who.to_string())
    }

    fn persist(&self) {
        let Some(path) = &self.core.path else { return };
        match self.core.save(None) {
            Ok(_) => self.state.lock().save_failed = false,
            Err(e) => {
                let mut st = self.state.lock();
                if !st.save_failed {
                    tracing::warn!("identity: could not save {}: {}", path.display(), e);
                }
                st.save_failed = true;
            }
        }
    }

    fn note(&self, who: &str, d: Option<f64>, now: f64) {
        let mut st = self.state.lock();
        st.last_distance = d;
        if st.last_who.as_deref() == Some(who) {
            return;
        }
        st.last_who = Some(who.to_string());
        if now - st.transition_logged_at < TRANSITION_LOG_SECS {
            return;
        }
        st.transition_logged_at = now;
        drop(st);
        let dist = d.map(|d| format!(" (d={d:.2})")).unwrap_or_default();
        if who == WHO_OWNER {
            tracing::info!("identity: owner recognised{}", dist);
        } else {
            tracing::info!("identity: unknown face{}", dist);
        }
    }

    /// The `identity` object the daemon reports over IPC.
    pub fn status(&self) -> Value {
        let (last_who, last_distance, enrolments) = {
            let st = self.state.lock();
            (st.last_who.clone(), st.last_distance, st.enrolments)
        };
        json!({
            "prints": self.core.len(),
            "capacity": self.core.capacity,
            "threshold": self.core.threshold,
            "path": self.core.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "enrolled_at": self.core.enrolled_at(),
            "last_who": last_who,
            "last_distance": last_distance,
            "enrolments": enrolments,
        })
    }
}

impl eve_vision::Identity for FaceIdentity {
    fn process(&self, frame: &Frame, rects: &[Rect]) -> anyhow::Result<String> {
        FaceIdentity::process(self, frame, rects)
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Face rect grown by [`CROP_PAD`] of its size on each side, clamped to the
/// image. Returns integer (x, y, w, h) in top-left pixel coordinates.
pub fn crop_rect(face: &Rect, width: f64, height: f64) -> (i64, i64, i64, i64) {
    crop_rect_padded(face, width, height, CROP_PAD)
}

/// [`crop_rect`] with an explicit `pad` (Python's keyword).
pub fn crop_rect_padded(face: &Rect, width: f64, height: f64, pad: f64) -> (i64, i64, i64, i64) {
    // Python's max(0.0, v) / min(width, v) keep the first argument unless the
    // second is strictly beyond it, so a NaN gives the bound; f64::max/min agree.
    let x0 = 0.0f64.max(face.x - face.w * pad);
    let y0 = 0.0f64.max(face.y - face.h * pad);
    let x1 = width.min(face.x + face.w * (1.0 + pad));
    let y1 = height.min(face.y + face.h * (1.0 + pad));
    let (x, y) = (x0.floor() as i64, y0.floor() as i64);
    let (w, h) = (x1.ceil() as i64 - x, y1.ceil() as i64 - y);
    (x, y, w.max(0), h.max(0))
}

// ---- platform adapter: macOS Vision ------------------------------------------

/// The Vision feature-print describer, or None (with one warning) when unavailable.
pub fn make_describer() -> Option<Describer> {
    #[cfg(target_os = "macos")]
    {
        mac::make_describer()
    }
    #[cfg(not(target_os = "macos"))]
    {
        tracing::warn!("identity: owner recognition needs macOS Vision — every face stays 'unknown'");
        None
    }
}

/// Worker thread. `VNGenerateImageFeaturePrintRequest` (revision 2) on the
/// padded face crop. The crop uses `CGImageCreateWithImageInRect`, whose rect
/// is in top-left pixel coordinates — the same space as [`Rect`] — so no flip.
pub fn vision_feature_print(frame: &Frame, face: &Rect) -> anyhow::Result<Option<Vector>> {
    #[cfg(target_os = "macos")]
    {
        mac::vision_feature_print(frame, face)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (frame, face);
        anyhow::bail!("No module named 'objc'")
    }
}

// ---- CLI ---------------------------------------------------------------------

/// `eve-bridge identity status|reset`.
///
/// Talks to the daemon over IPC when it is running (it holds the prints in
/// memory); otherwise works on the file directly. Prints to stdout; returns
/// the exit code. An exception Python would raise prints its last traceback
/// line to stderr and exits 1.
pub fn run_identity(action: &str, socket_path: Option<&str>) -> i32 {
    let path = default_path();
    let mut post = |event: Value| {
        eve_hooks::client::post(&event, socket_path, 3.0, eve_hooks::client::DEFAULT_TIMEOUT_SECS)
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match run_identity_with(action, &path, configured_threshold, &mut post, &mut out) {
        Ok(code) => code,
        Err(e) => {
            let _ = out.flush();
            eprintln!("Traceback (most recent call last):\n{}: {}", e.kind, e.message);
            1
        }
    }
}

fn write_err(e: io::Error) -> HookError {
    HookError::new(eve_ipc::io_error_type_name(&e), eve_ipc::py_os_error_str(&e))
}

/// `resp.get(key)` on the daemon's reply: a reply that is not a dict raises
/// AttributeError, as in Python.
fn resp_get<'a>(resp: &'a Value, key: &str) -> Result<Option<&'a Value>, HookError> {
    match resp {
        Value::Object(m) => Ok(m.get(key)),
        other => Err(HookError::no_attribute(other, "get")),
    }
}

/// [`run_identity`] with its seams: the store path, the threshold lookup
/// (only called when the file is read for status), the IPC post and stdout.
pub fn run_identity_with(
    action: &str,
    path: &Path,
    threshold: impl FnOnce() -> f64,
    post: &mut dyn FnMut(Value) -> Option<Value>,
    out: &mut dyn io::Write,
) -> Result<i32, HookError> {
    let resp = post(json!({"evt": "identity", "action": action}));
    if action == "reset" {
        if let Some(resp) = &resp {
            let removed = resp_get(resp, "removed")?.cloned().unwrap_or(json!(0));
            writeln!(out, "owner prints reset ({} removed) — {}", eve_hooks::py_str(&removed), path.display())
                .map_err(write_err)?;
            return Ok(0);
        }
        let core = OwnerIdentity::new().with_path(path);
        let n = core.load(None);
        core.reset().map_err(write_err)?;
        writeln!(
            out,
            "daemon not reachable; removed {} ({} prints). A running daemon keeps its in-memory prints until restart.",
            path.display(),
            n
        )
        .map_err(write_err)?;
        return Ok(0);
    }
    if let Some(resp) = &resp {
        if let Some(Value::Object(st)) = resp_get(resp, "identity")? {
            let enabled = resp_get(resp, "enabled")?.is_some_and(eve_hooks::py_truthy);
            print_status(st, true, Some(enabled), out).map_err(write_err)?;
            return Ok(0);
        }
    }
    let core = OwnerIdentity::new().with_path(path).with_threshold(threshold());
    core.load(None);
    let describe: Describer = Arc::new(|_, _| Ok(None));
    let st = FaceIdentity::new(Arc::new(core), describe).status();
    let empty = Map::new();
    print_status(st.as_object().unwrap_or(&empty), false, None, out).map_err(write_err)?;
    Ok(0)
}

/// `isinstance(v, (int, float))` (bools included) as a float.
fn py_number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn print_status(st: &Map<String, Value>, live: bool, enabled: Option<bool>, out: &mut dyn io::Write) -> io::Result<()> {
    let get = |k: &str, default: Value| eve_hooks::py_str(st.get(k).unwrap_or(&default));
    let src = if live { "daemon" } else { "file (daemon not reachable)" };
    writeln!(out, "owner prints: {}/{}  [{}]", get("prints", json!(0)), get("capacity", json!(CAPACITY)), src)?;
    writeln!(out, "file:         {}", get("path", Value::Null))?;
    writeln!(out, "threshold:    {}  (EVE_OWNER_THRESHOLD)", get("threshold", Value::Null))?;
    if let Some(at) = py_number(st.get("enrolled_at")) {
        use chrono::TimeZone as _;
        // time.localtime floors a float timestamp to the second.
        if let chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) =
            chrono::Local.timestamp_opt(at.floor() as i64, 0)
        {
            writeln!(out, "last enrol:   {}", t.format("%Y-%m-%d %H:%M:%S"))?;
        }
    }
    if live {
        let on = if enabled.unwrap_or(false) { "on" } else { "off (no Vision on this host)" };
        writeln!(out, "recognition:  {on}")?;
        if let Some(who) = st.get("last_who").filter(|w| !w.is_null()) {
            let d = py_number(st.get("last_distance")).map(|d| format!(" (d={d:.2})")).unwrap_or_default();
            writeln!(out, "last face:    {}{}", eve_hooks::py_str(who), d)?;
        }
        writeln!(out, "enrolments:   {} this run", get("enrolments", json!(0)))?;
    }
    if !st.get("prints").is_some_and(eve_hooks::py_truthy) {
        writeln!(out, "\nto enrol: hold Option while facing the robot for a few seconds")?;
    }
    Ok(())
}
