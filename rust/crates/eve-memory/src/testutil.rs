//! What the Python tests had as fixtures and fakes: a fixed local clock, the fake records module, the fake mem0
//! index, the token clock, a Memory over a temp folder, and a captured log.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone};
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_records::RecallConfig;
use eve_transcripts::transcripts::{Append, Dt, Pred, TranscriptConfig, Transcripts};

use crate::memory::{Clock, MeaningIndex, Memory, MemoryOptions, RecordsStore};

pub const SENTINEL: &str = "zebra-sentinel-7Q";

pub fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// `datetime(2026, 9, 21, 12, 0).astimezone().tzinfo`: the local offset on that day, fixed.
pub fn tz_on(y: i32, m: u32, d: u32) -> FixedOffset {
    let naive = NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(12, 0, 0).unwrap();
    *Local.from_local_datetime(&naive).earliest().unwrap().offset()
}

/// `datetime(y, m, d, hh, mm, tzinfo=tz)`.
pub fn at_tz(tz: FixedOffset, y: i32, m: u32, d: u32, hh: u32, mm: u32) -> Dt {
    let naive = NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(hh, mm, 0).unwrap();
    tz.from_local_datetime(&naive).single().unwrap()
}

/// test_memory.py's `at(day, hh, mm)`: September 2026 in the September offset.
pub fn at(day: u32, hh: u32, mm: u32) -> Dt {
    at_tz(tz_on(2026, 9, 21), 2026, 9, day, hh, mm)
}

pub fn no_env() -> Option<HashMap<String, String>> {
    Some(HashMap::new())
}

pub fn env_of(pairs: &[(&str, &str)]) -> Option<HashMap<String, String>> {
    Some(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

pub fn say(tx: &Transcripts, ch: &str, conv: &str, who: &str, text: &str, when: Dt) {
    assert!(tx.append_with(ch, conv, who, "say", text, Append { now: Some(when), ..Append::default() }));
}

pub fn write(path: &Path, text: &str, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.to_path_buf()
}

pub fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap()
}

// ---- the fake records module ----------------------------------------------------------------------

/// What the fake records saw: `("star", text, when)`, `("forget_lines", dry_run)`, `("squash", repo)`,
/// `("seal", repo)`.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Star(String, Option<NaiveDateTime>),
    ForgetLines(bool),
    Squash(PathBuf),
    Seal(PathBuf),
}

#[derive(Default)]
pub struct FakeState {
    pub lines: Vec<String>,
    pub calls: Vec<Call>,
    pub fail: bool,
    pub star_fails: bool,
    pub search_panics: bool,
}

/// records.py as the contract names it, kept in memory, with a real starred.md so the move is checkable. It has
/// no hidden list and no talking section: those calls fail, as the Python fake's missing attributes do.
#[derive(Default)]
pub struct FakeRecords {
    pub state: Mutex<FakeState>,
}

impl FakeRecords {
    pub fn new(lines: &[&str]) -> Arc<FakeRecords> {
        let rec = FakeRecords::default();
        rec.state.lock().lines = lines.iter().map(|s| s.to_string()).collect();
        Arc::new(rec)
    }

    /// The fixture: two record lines, one with the sentinel.
    pub fn fixture() -> Arc<FakeRecords> {
        FakeRecords::new(&[
            &format!("Has a cat called {SENTINEL} (said 2026-09-20)."),
            "Likes tea (said 2026-09-19).",
        ])
    }

    fn check(&self) -> anyhow::Result<()> {
        if self.state.lock().fail {
            anyhow::bail!("records broke");
        }
        Ok(())
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().calls.clone()
    }

    pub fn lines(&self) -> Vec<String> {
        self.state.lock().lines.clone()
    }
}

fn no_attribute(name: &str) -> anyhow::Error {
    anyhow::anyhow!("'FakeRecords' object has no attribute '{name}'")
}

impl RecordsStore for FakeRecords {
    fn profile(&self, _cfg: &RecallConfig) -> anyhow::Result<String> {
        self.check()?;
        Ok("PROFILE".into())
    }

    fn profile_for_voice(&self, _cfg: &RecallConfig) -> anyhow::Result<String> {
        self.check()?;
        Ok("VOICE PROFILE".into())
    }

    fn talking_section(&self, _cfg: &RecallConfig) -> anyhow::Result<String> {
        Err(no_attribute("talking_section"))
    }

    fn star(&self, cfg: &RecallConfig, text: &str, when: Option<NaiveDateTime>) -> anyhow::Result<Option<String>> {
        self.check()?;
        if self.state.lock().star_fails {
            return Err(io::Error::other("disk").into());
        }
        self.state.lock().calls.push(Call::Star(text.into(), when));
        std::fs::create_dir_all(cfg.records_dir())?;
        let path = cfg.records_dir().join("starred.md");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.to_lowercase().contains(&text.to_lowercase()) {
            return Ok(None);
        }
        let day = when.unwrap_or_else(|| Local::now().naive_local()).format("%Y-%m-%d");
        let line = format!("- {text} ({day})");
        std::fs::write(&path, format!("{existing}{line}\n"))?;
        Ok(Some(line))
    }

    fn hide(&self, _cfg: &RecallConfig, _text: &str) -> anyhow::Result<Option<String>> {
        Err(no_attribute("hide"))
    }

    fn unhide(&self, _cfg: &RecallConfig, _text: &str) -> anyhow::Result<bool> {
        Err(no_attribute("unhide"))
    }

    fn hidden(&self, _cfg: &RecallConfig) -> anyhow::Result<Vec<String>> {
        Err(no_attribute("hidden"))
    }

    fn hidden_match(&self, _cfg: &RecallConfig) -> anyhow::Result<Option<Pred>> {
        Err(no_attribute("hidden_match"))
    }

    fn search(&self, _cfg: &RecallConfig, query: &str) -> anyhow::Result<Value> {
        self.check()?;
        if self.state.lock().search_panics {
            panic!("division by zero"); // the Python test's 1 / 0
        }
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        let hits: Vec<Value> = self
            .state
            .lock()
            .lines
            .iter()
            .filter(|ln| words.iter().all(|w| ln.to_lowercase().contains(w.as_str())))
            .map(|ln| json!({"id": "pets", "line": ln}))
            .collect();
        Ok(json!({"ok": true, "hits": hits, "recalled": []}))
    }

    fn get(&self, _cfg: &RecallConfig, rid: &str) -> anyhow::Result<Value> {
        self.check()?;
        if rid == "pets" {
            Ok(json!({"ok": true, "record": format!("record {rid}")}))
        } else {
            Ok(json!({"ok": false, "reason": format!("no record {}", eve_records::py::repr_str(rid))}))
        }
    }

    fn forget_lines(&self, _cfg: &RecallConfig, pred: &dyn Fn(&str) -> bool, dry_run: bool) -> anyhow::Result<usize> {
        self.check()?;
        let mut st = self.state.lock();
        let hit = st.lines.iter().filter(|ln| pred(ln)).count();
        st.calls.push(Call::ForgetLines(dry_run));
        if !dry_run {
            st.lines.retain(|ln| !pred(ln));
        }
        Ok(hit)
    }

    fn squash_history(&self, repo: &Path) -> anyhow::Result<bool> {
        self.state.lock().calls.push(Call::Squash(repo.to_path_buf()));
        Ok(true)
    }

    fn seal(&self, repo: &Path) -> anyhow::Result<()> {
        self.state.lock().calls.push(Call::Seal(repo.to_path_buf()));
        Ok(())
    }
}

// ---- the fake mem0 index ------------------------------------------------------------------------------

pub struct Row {
    pub id: String,
    pub text: String,
    pub day: String,
    pub channel: String,
}

pub fn row(id: &str, text: &str, day: &str, channel: &str) -> Row {
    Row { id: id.into(), text: text.into(), day: day.into(), channel: channel.into() }
}

#[derive(Default)]
pub struct FakeIndex {
    pub rows: Mutex<Vec<Row>>,
    pub forgot: Mutex<Vec<String>>,
    /// A forget_matching that fails, as mem0 in its retry window (or with no key) does.
    pub broken: bool,
}

impl FakeIndex {
    pub fn new(rows: Vec<Row>) -> Arc<FakeIndex> {
        Arc::new(FakeIndex { rows: Mutex::new(rows), ..FakeIndex::default() })
    }

    pub fn ids(&self) -> Vec<String> {
        self.rows.lock().iter().map(|r| r.id.clone()).collect()
    }
}

impl MeaningIndex for FakeIndex {
    fn search(&self, query: &str) -> anyhow::Result<Vec<Value>> {
        let first = query.split_whitespace().next().unwrap_or("").to_lowercase();
        Ok(self
            .rows
            .lock()
            .iter()
            .filter(|r| r.text.to_lowercase().contains(&first))
            .map(|r| json!({"text": r.text, "day": r.day, "channel": r.channel}))
            .collect())
    }

    fn find(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<Vec<Value>> {
        Ok(self.rows.lock().iter().filter(|r| pred(&r.text)).map(|r| json!({"id": r.id, "text": r.text})).collect())
    }

    fn forget(&self, ids: &[String]) -> anyhow::Result<usize> {
        self.forgot.lock().extend(ids.iter().cloned());
        let mut rows = self.rows.lock();
        let before = rows.len();
        rows.retain(|r| !ids.contains(&r.id));
        Ok(before - rows.len())
    }

    fn forget_matching(&self, _pred: &dyn Fn(&str) -> bool) -> Option<anyhow::Result<usize>> {
        if self.broken {
            // mem0 could not be opened: OwnerMemory raises instead of answering 0
            return Some(Err(anyhow::anyhow!("mem0 is unavailable")));
        }
        None
    }
}

// ---- the token clock --------------------------------------------------------------------------------

#[derive(Clone)]
pub struct TestClock(pub Arc<Mutex<f64>>);

impl TestClock {
    pub fn new() -> TestClock {
        TestClock(Arc::new(Mutex::new(1000.0)))
    }

    pub fn advance(&self, secs: f64) {
        *self.0.lock() += secs;
    }

    pub fn clock(&self) -> Clock {
        let t = self.0.clone();
        Arc::new(move || *t.lock())
    }
}

// ---- a Memory over a temp folder ----------------------------------------------------------------------

pub struct Built {
    pub memory: Memory,
    pub tx: Arc<Transcripts>,
    pub cfg: RecallConfig,
}

pub fn transcripts(cfg: &RecallConfig, now: Dt) -> Arc<Transcripts> {
    let tx = Transcripts::new(
        TranscriptConfig::new(true, cfg.transcripts_dir()),
        Some(cfg.archive_dir()),
        Some(Arc::new(move || now)),
    );
    assert!(tx.enabled);
    Arc::new(tx)
}

/// test_memory.py's `build`: a Memory over `<tmp>/memory`, every switch off.
pub fn build(
    tmp: &Path,
    index: Option<Arc<dyn MeaningIndex>>,
    clock: Option<Clock>,
    records: Option<Arc<dyn RecordsStore>>,
    now: Dt,
) -> Built {
    let cfg = RecallConfig::new(tmp.join("memory"), tmp.join("notes"));
    let tx = transcripts(&cfg, now);
    let opts = MemoryOptions { clock, records, environ: no_env(), ..MemoryOptions::default() };
    let memory = Memory::with_options(cfg.clone(), tx.clone(), index, opts);
    Built { memory, tx, cfg }
}

/// test_memory.py's `seed`.
pub fn seed(tx: &Transcripts) {
    say(tx, "voice", "v-1758448800000-aaaa", "owner", &format!("my cat {SENTINEL} is asleep"), at(21, 9, 0));
    say(tx, "voice", "v-1758448800000-aaaa", "eve", "Sweet dreams to the cat.", at(21, 9, 1));
    say(tx, "telegram", "t-1758459600000-bbbb", "owner", "book the dentist", at(21, 11, 0));
    say(tx, "telegram", "t-1758459600000-bbbb", "eve", "Booked for Friday.", at(21, 11, 1));
}

// ---- the captured log -----------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// caplog at DEBUG: what `f` logs on this thread, as text.
pub fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    let sink = Sink::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(sink.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&sink.0.lock()).into_owned();
    (out, text)
}
