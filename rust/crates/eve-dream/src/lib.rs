//! Port of `dream.py`. The nightly dream: once a day, eve rereads what was said and fixes what it knows.
//!
//! The owner asked for it on 2026-09-23: "dream every night as well so that memories can be fixed and updated". Until
//! then a fact reached the records only through a chain of per-conversation distils, a curate pass and a reconcile
//! loop, and a wrong fact stayed wrong until the owner edited the file by hand. Memory is now three stores (owner,
//! 2026-09-23): the transcripts hold every word, the records hold what is true, mem0 finds things by meaning. The
//! dream is the one step that turns the first into the other two.
//!
//! ### What one night is
//!
//! For one transcript day, in a worker thread, never on the event loop:
//!
//! 1. `records::reconcile_day` — ONE model call reads the current records, the profile, the owner's stars and the
//!    whole day's transcript, and returns the records that change, a new profile, and a journal (`days/<day>.md`).
//!    Where the day contradicts a record, the record gets a dated correction (with `EVE_MEMORY_VALIDITY=1`: the old
//!    fact is closed by its `valid_to`, at most `records::MAX_CLOSES_PER_NIGHT` a night, or the night is refused and
//!    dreamt again). That is the "fixed and updated".
//! 2. `records::consolidate` — ONE model call over all records merges two ids for the same thing and ages out a fact
//!    a newer one replaced.
//! 3. `index.ingest_day` + `index.tidy` — the day's conversations go into mem0, and exact duplicates and ★
//!    candidates come out.
//! 4. The day goes into `records/.dreamt` (one day per line) and a `## Dream` section with the counts is appended to
//!    the journal, then committed, so every night ends with its whole journal in the records' history.
//!
//! ### Why the dream is the only automatic writer of records
//!
//! The records keep one rule from their first day (2026-09-20): the brains read, they never write. A model that
//! promotes its own mid-conversation conclusions into what it "knows" launders a guess into a fact. So nothing that
//! talks writes a record. The owner's own "remember that" goes to `starred.md`, and the dream — reading the owner's
//! actual words, a day later, with the current records as input so hand edits survive — is the only thing that
//! changes a record by itself.
//!
//! ### Why after 04:00, and why at 04:30
//!
//! A transcript day ends at 04:00, not midnight, so a conversation at 01:00 belongs to the evening it continued. A
//! night is dreamt only once its day is over: never the current transcript day. The just-ended day waits until 04:30
//! local time (`DREAM_HOUR`), half an hour past the rollover, so a conversation that ran over 04:00 has closed. Older
//! nights are due at any time: that is the catch-up after the Mac slept through 04:30. Catch-up runs oldest first, at
//! most `MAX_NIGHTS_PER_WAKE` a wake, so a week away does not become one long burst of model calls.
//!
//! ### Failure is silence
//!
//! The dream never breaks a conversation and never returns an error out of [`Dreamer::run_loop`]. When the reconcile
//! call fails, nothing is written and the night is not marked: the next wake tries again, and the warning is logged
//! once per night. A night that fails `MAX_TRIES` times in one run of the daemon is set aside until the next start,
//! so one bad night cannot hold up the nights after it. When consolidate or the index fails, the night is still
//! marked: both are retried by the next night's dream anyway (consolidate reads all records; the index ingests each
//! conversation it has not seen). A day with nothing said is marked with no model call. Logs carry days, counts and
//! seconds, never a word that was said.
//!
//! ### A forget during a dream
//!
//! The model call runs for minutes with the records unlocked. A forget that lands meanwhile would be undone by the
//! night's write — the model answered from the words before the forget — and the commit would put them back into the
//! history the forget had just squashed. So the night pins the forget generation before it reads the day, and
//! reconcile and consolidate write nothing when it moved: the night counts as a failed reconcile and is dreamt again
//! next wake, from what is left. mem0 guards its own adds the same way.

use std::collections::{HashMap, HashSet};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Local, NaiveDateTime, Timelike};
use eve_mem0_memory::OwnerMemory;
use eve_records::{DayChange, DreamClient, RecallConfig};
use eve_transcripts::{py as tpy, Transcripts};
use parking_lot::Mutex;
use regex::Regex;
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// 04:30 local: half an hour after the 04:00 day rollover.
pub const DREAM_HOUR: f64 = 4.5;
/// Catch-up is oldest first, at most this many nights a wake.
pub const MAX_NIGHTS_PER_WAKE: usize = 3;
/// Failed reconciles of one night before it waits for the next daemon start.
pub const MAX_TRIES: u32 = 3;
/// The first look after start: soon, but not in the middle of start-up.
pub const FIRST_WAKE_SECS: f64 = 120.0;
/// `<records>/.dreamt`: one dreamt day per line.
pub const DREAMT_FILE: &str = ".dreamt";
/// `<records>/days/<day>.md`: the journal reconcile_day writes.
pub const DAYS_SUBDIR: &str = "days";
pub const REPORT_HEADING: &str = "## Dream";
/// The loop's default interval between wakes (`loop(shutdown, interval_secs=1800.0)`).
pub const LOOP_INTERVAL_SECS: f64 = 1800.0;

/// `re.match(r"^\d{4}-\d{2}-\d{2}$", s)`: Python's `$` also matches before one final newline, and `\d` is any
/// Unicode decimal digit in both engines.
static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}\n?\z").expect("day pattern"));

/// What one night did. Counts only: a report never carries what was said.
#[derive(Debug, Clone, PartialEq)]
pub struct DreamReport {
    pub day: String,
    /// marked in .dreamt
    pub dreamt: bool,
    /// nothing was said: marked with no model call
    pub empty: bool,
    /// records the reconcile changed
    pub changed: usize,
    /// of those, new records
    pub created: usize,
    /// dated corrections the day forced
    pub corrections: usize,
    /// facts closed by a valid_to (EVE_MEMORY_VALIDITY)
    pub closed: usize,
    /// records consolidate rewrote
    pub consolidated: usize,
    /// records consolidate merged away
    pub removed: usize,
    /// memories the index added
    pub ingested: usize,
    /// memories the index removed
    pub tidied: usize,
    /// steps that failed: read, reconcile, consolidate, index, mark (and dream, the last net)
    pub failed: Vec<String>,
    pub secs: f64,
    /// the day's journal the counts were appended to
    pub journal: Option<PathBuf>,
}

impl DreamReport {
    pub fn new(day: impl Into<String>) -> Self {
        DreamReport {
            day: day.into(),
            dreamt: false,
            empty: false,
            changed: 0,
            created: 0,
            corrections: 0,
            closed: 0,
            consolidated: 0,
            removed: 0,
            ingested: 0,
            tidied: 0,
            failed: Vec::new(),
            secs: 0.0,
            journal: None,
        }
    }
}

/// A moment the dream is asked about: an aware datetime, or a naive one (taken as local already), as Python's
/// `due_nights(now)` accepts either.
#[derive(Debug, Clone, Copy)]
pub enum When {
    Aware(DateTime<FixedOffset>),
    Naive(NaiveDateTime),
}

impl From<DateTime<FixedOffset>> for When {
    fn from(d: DateTime<FixedOffset>) -> Self {
        When::Aware(d)
    }
}

impl From<DateTime<Local>> for When {
    fn from(d: DateTime<Local>) -> Self {
        When::Aware(d.fixed_offset())
    }
}

impl From<NaiveDateTime> for When {
    fn from(d: NaiveDateTime) -> Self {
        When::Naive(d)
    }
}

/// `_local(now)`: the same moment on this Mac's wall clock. A naive datetime is taken as local already.
fn local(now: When) -> DateTime<FixedOffset> {
    match now {
        When::Aware(d) => d.with_timezone(&Local).fixed_offset(),
        When::Naive(n) => tpy::local_aware(n),
    }
}

/// The local clock (`datetime.now` in Python).
pub type Wall = Arc<dyn Fn() -> DateTime<FixedOffset> + Send + Sync>;

/// The records steps one night calls: `records.reconcile_day`, `records.consolidate` and `records.commit`. The
/// daemon uses [`LiveRecords`]; a test passes a fake (Python's tests monkeypatch the module functions). An `Err` is
/// a step that raised.
pub trait NightRecords: Send + Sync {
    fn reconcile_day(
        &self,
        cfg: &RecallConfig,
        client: &dyn DreamClient,
        day: &str,
        day_text: &str,
    ) -> anyhow::Result<Option<DayChange>>;
    fn consolidate(&self, cfg: &RecallConfig, client: &dyn DreamClient) -> anyhow::Result<(usize, usize)>;
    fn commit(&self, repo: &Path, message: &str) -> Option<String>;
}

/// The real records steps.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveRecords;

impl NightRecords for LiveRecords {
    fn reconcile_day(
        &self,
        cfg: &RecallConfig,
        client: &dyn DreamClient,
        day: &str,
        day_text: &str,
    ) -> anyhow::Result<Option<DayChange>> {
        Ok(eve_records::reconcile_day(cfg, client, day, day_text))
    }

    fn consolidate(&self, cfg: &RecallConfig, client: &dyn DreamClient) -> anyhow::Result<(usize, usize)> {
        Ok(eve_records::consolidate(cfg, client))
    }

    fn commit(&self, repo: &Path, message: &str) -> Option<String> {
        eve_records::records::commit(repo, message)
    }
}

/// The mem0 index as the dream uses it (`OwnerMemory`, or a fake): feed it one day, then tidy it. An `Err` is a
/// step that raised.
pub trait DreamIndex: Send + Sync {
    fn ingest_day(&self, transcripts: &Transcripts, day: &str) -> anyhow::Result<usize>;
    fn tidy(&self) -> anyhow::Result<usize>;
}

impl DreamIndex for OwnerMemory {
    fn ingest_day(&self, transcripts: &Transcripts, day: &str) -> anyhow::Result<usize> {
        Ok(OwnerMemory::ingest_day(self, transcripts, day))
    }

    fn tidy(&self) -> anyhow::Result<usize> {
        Ok(OwnerMemory::tidy(self))
    }
}

/// Python's `UnicodeEncodeError` from `f"{day}\n".encode("ascii")`: a day of non-ASCII digits passes the day
/// pattern but cannot be written to the ledger, and the night stops there.
#[derive(Debug, thiserror::Error)]
#[error("'ascii' codec can't encode the day")]
pub struct LedgerEncodeError;

/// `type(e).__name__` for a log line.
fn error_name(e: &anyhow::Error) -> String {
    if e.downcast_ref::<LedgerEncodeError>().is_some() {
        return "UnicodeEncodeError".into();
    }
    eve_records::py::error_name(e)
}

/// What one run of the daemon shares between the loop and the worker threads.
#[derive(Default)]
struct Shared {
    night_lock: Mutex<()>,          // one night at a time, whoever asks
    tries: Mutex<HashMap<String, u32>>, // failed reconciles per night, this run of the daemon
    loop_warned: AtomicBool,
}

/// The nightly pass. `client` is the records' reconcile client ([`eve_records::OpenAIReconcileClient`] in the
/// daemon, a fake in tests); `index` is the mem0 index or None; `wall` is the local clock.
pub struct Dreamer {
    pub cfg: RecallConfig,
    pub transcripts: Arc<Transcripts>,
    pub client: Arc<dyn DreamClient + Send + Sync>,
    pub index: Option<Arc<dyn DreamIndex>>,
    pub wall: Wall,
    pub records: Arc<dyn NightRecords>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Dreamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dreamer")
            .field("cfg", &self.cfg)
            .field("transcripts", &self.transcripts)
            .field("index", &self.index.is_some())
            .finish_non_exhaustive()
    }
}

impl Dreamer {
    /// `Dreamer(cfg, transcripts, client, index, wall=datetime.now)`. `wall` None is the local clock.
    pub fn new(
        cfg: RecallConfig,
        transcripts: Arc<Transcripts>,
        client: Arc<dyn DreamClient + Send + Sync>,
        index: Option<Arc<dyn DreamIndex>>,
        wall: Option<Wall>,
    ) -> Self {
        Dreamer {
            cfg,
            transcripts,
            client,
            index,
            wall: wall.unwrap_or_else(|| Arc::new(tpy::now_local)),
            records: Arc::new(LiveRecords),
            shared: Arc::new(Shared::default()),
        }
    }

    /// The same dreamer with other records steps (a test's fakes).
    pub fn with_records(mut self, records: Arc<dyn NightRecords>) -> Self {
        self.records = records;
        self
    }

    /// The same dreamer, sharing its lock and its tries: what a worker thread runs on.
    fn handle(&self) -> Dreamer {
        Dreamer {
            cfg: self.cfg.clone(),
            transcripts: self.transcripts.clone(),
            client: self.client.clone(),
            index: self.index.clone(),
            wall: self.wall.clone(),
            records: self.records.clone(),
            shared: self.shared.clone(),
        }
    }

    // -- the ledger ---------------------------------------------------------------------------

    pub fn dreamt_path(&self) -> PathBuf {
        self.cfg.records_dir().join(DREAMT_FILE)
    }

    /// Every day already dreamt. A missing or unreadable ledger is an empty one.
    pub fn dreamt(&self) -> HashSet<String> {
        let Ok(raw) = fs::read_to_string(self.dreamt_path()) else {
            return HashSet::new();
        };
        tpy::splitlines(&raw)
            .into_iter()
            .map(tpy::strip)
            .filter(|s| DAY.is_match(s))
            .map(str::to_string)
            .collect()
    }

    /// Append the day to the ledger: one write of one whole line on an O_APPEND descriptor. Err only for a day
    /// that is not ASCII (Python's UnicodeEncodeError, which no step catches).
    fn mark(&self, day: &str) -> anyhow::Result<bool> {
        if self.dreamt().contains(day) {
            return Ok(true);
        }
        let folder = self.cfg.records_dir();
        let res = (|| -> std::io::Result<()> {
            make_dir(&folder)?;
            let mut f = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(self.dreamt_path())?;
            if !day.is_ascii() {
                return Err(std::io::Error::other(LedgerEncodeError));
            }
            f.write_all(format!("{day}\n").as_bytes())
        })();
        match res {
            Ok(()) => Ok(true),
            Err(e) if e.get_ref().is_some_and(|inner| inner.is::<LedgerEncodeError>()) => Err(LedgerEncodeError.into()),
            Err(e) => {
                warn!(
                    "dream: could not mark {day} dreamt ({}) — it is dreamt again next wake",
                    tpy::os_error_name(&e)
                );
                Ok(false)
            }
        }
    }

    // -- which nights ---------------------------------------------------------------------------

    /// Transcript days before the current one, not yet dreamt, oldest first, at most MAX_NIGHTS_PER_WAKE.
    ///
    /// The just-ended calendar day waits until DREAM_HOUR; any older night is due at once (catch-up). A night that
    /// failed MAX_TRIES times in this run waits for the next daemon start.
    pub fn due_nights(&self, now: impl Into<When>) -> Vec<String> {
        let local = local(now.into());
        let current = self.transcripts.day_of(local);
        let yesterday = (local.date_naive() - ChronoDuration::days(1)).format("%Y-%m-%d").to_string();
        let past_hour = f64::from(local.hour()) + f64::from(local.minute()) / 60.0 >= DREAM_HOUR;
        let done = self.dreamt();
        let tries = self.shared.tries.lock().clone();
        let mut due = Vec::new();
        for day in self.transcripts.days() {
            if day >= current || done.contains(&day) || tries.get(&day).copied().unwrap_or(0) >= MAX_TRIES {
                continue;
            }
            if day >= yesterday && !past_hour {
                continue;
            }
            due.push(day);
            if due.len() >= MAX_NIGHTS_PER_WAKE {
                break;
            }
        }
        due
    }

    // -- one night -----------------------------------------------------------------------------

    /// Dream one night, in a worker thread. Never fails: a failure is in `report.failed`.
    pub async fn dream(&self, day: &str) -> DreamReport {
        let me = self.handle();
        let owned = day.to_string();
        // The worker logs to whatever the caller logs to (a test's capture, or the daemon's global subscriber).
        let dispatch = tracing::dispatcher::get_default(|d| d.clone());
        let res = tokio::task::spawn_blocking(move || {
            tracing::dispatcher::with_default(&dispatch, || me.night(&owned))
        })
        .await;
        let why = match res {
            Ok(Ok(report)) => return report,
            Ok(Err(e)) => error_name(&e),
            Err(join) if join.is_panic() => "panic".to_string(),
            Err(_) => "CancelledError".to_string(),
        };
        // the thread already guards each step; this is the last net
        warn!("dream: {day} stopped ({why})");
        let mut report = DreamReport::new(day);
        report.failed.push("dream".into());
        report
    }

    /// One night, on the calling thread (blocking: model calls run for minutes). [`Dreamer::dream`] runs it on a
    /// worker thread; call it directly only off the async runtime. Err only where Python's `_night` raises.
    pub fn night(&self, day: &str) -> anyhow::Result<DreamReport> {
        let _night = self.shared.night_lock.lock();
        let started = Instant::now();
        let mut report = DreamReport::new(day);
        // Pinned before the day is read: a forget from here on makes the night's writes drop, since the model
        // would answer from words that are forgotten by then (verification/Eve/Forget.lean).
        eve_records::forgets_pinned(&self.cfg, || self.run(day, &mut report))?;
        report.secs = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        if report.dreamt && !report.empty {
            self.append_report(day, &mut report);
            if report.journal.is_some() {
                // The counts land after reconcile's and consolidate's commits: one more, so the night ends with
                // its whole journal in the records' history, never as an uncommitted edit.
                self.records.commit(&self.cfg.records_dir(), &format!("dream: {day} report"));
            }
        }
        if report.dreamt {
            let what = if report.empty { "nothing said, no call".to_string() } else { Self::summary(&report) };
            info!("dream: {day} — {what}");
        }
        Ok(report)
    }

    fn run(&self, day: &str, report: &mut DreamReport) -> anyhow::Result<()> {
        if !DAY.is_match(day) || !self.transcripts.enabled {
            report.failed.push("read".into()); // a night that was never read is never marked
            return Ok(());
        }
        let text = self.transcripts.day_text(day, eve_transcripts::transcripts::DAY_TEXT_CHARS as i64, false);
        if tpy::strip(&text).is_empty() {
            report.empty = true;
            report.dreamt = self.mark(day)?;
            if !report.dreamt {
                report.failed.push("mark".into());
            }
            return Ok(());
        }

        // reconcile_day promises None on failure; hold it to that
        let change = match self.records.reconcile_day(&self.cfg, self.client.as_ref(), day, &text) {
            Ok(c) => c,
            Err(e) => {
                debug!("dream: reconcile of {day} raised {}", error_name(&e));
                None
            }
        };
        let Some(change) = change else {
            let tries = {
                let mut all = self.shared.tries.lock();
                let n = all.entry(day.to_string()).or_insert(0);
                *n += 1;
                *n
            };
            report.failed.push("reconcile".into());
            if tries == 1 {
                warn!("dream: could not reconcile {day} — nothing written, it is tried again next wake");
            } else if tries >= MAX_TRIES {
                warn!("dream: {day} failed {tries} times — it waits for the next start");
            }
            return Ok(());
        };
        self.shared.tries.lock().remove(day);
        report.changed = change.changed;
        report.created = change.created;
        report.corrections = change.corrections;
        report.closed = change.closed;
        report.journal = (!change.journal.as_os_str().is_empty()).then(|| change.journal.clone());

        match self.records.consolidate(&self.cfg, self.client.as_ref()) {
            Ok((consolidated, removed)) => {
                report.consolidated = consolidated;
                report.removed = removed;
            }
            Err(e) => {
                report.failed.push("consolidate".into());
                warn!(
                    "dream: consolidate failed after {day} ({}) — the next night tries again",
                    error_name(&e)
                );
            }
        }

        if let Some(index) = &self.index {
            let res = index.ingest_day(&self.transcripts, day).and_then(|ingested| {
                report.ingested = ingested;
                index.tidy()
            });
            match res {
                Ok(tidied) => report.tidied = tidied,
                Err(e) => {
                    report.failed.push("index".into());
                    warn!("dream: the index missed {day} ({}) — the next night tries again", error_name(&e));
                }
            }
        }

        report.dreamt = self.mark(day)?;
        if !report.dreamt {
            report.failed.push("mark".into());
        }
        Ok(())
    }

    /// `Dreamer._summary`: the one line a night logs (and the CLI prints).
    pub fn summary(report: &DreamReport) -> String {
        let closed = if report.closed != 0 { format!(", {} closed", report.closed) } else { String::new() };
        let mut text = format!(
            "{} records changed ({} new, {} corrections{closed}); consolidate {} changed, {} removed; index {} added, \
             {} tidied; {:.1} s",
            report.changed,
            report.created,
            report.corrections,
            report.consolidated,
            report.removed,
            report.ingested,
            report.tidied,
            report.secs
        );
        if !report.failed.is_empty() {
            text += &format!("; failed: {}", report.failed.join(", "));
        }
        text
    }

    /// The counts, under the journal reconcile_day just wrote. No journal, no section.
    fn append_report(&self, day: &str, report: &mut DreamReport) {
        let journal = report
            .journal
            .clone()
            .unwrap_or_else(|| self.cfg.records_dir().join(DAYS_SUBDIR).join(format!("{day}.md")));
        if !journal.is_file() {
            report.journal = None;
            return;
        }
        let mut lines = vec![
            String::new(),
            REPORT_HEADING.to_string(),
            String::new(),
            format!(
                "- records: {} changed, {} new, {} corrections",
                report.changed, report.created, report.corrections
            ) + &if report.closed != 0 { format!(", {} closed", report.closed) } else { String::new() },
            format!("- consolidated: {} changed, {} removed", report.consolidated, report.removed),
            if self.index.is_some() {
                format!("- index: {} added, {} tidied", report.ingested, report.tidied)
            } else {
                "- index: off".to_string()
            },
            format!("- took {:.1} s", report.secs),
        ];
        if !report.failed.is_empty() {
            lines.push(format!("- failed: {}", report.failed.join(", ")));
        }
        let res = (|| -> std::io::Result<()> {
            let mut f = OpenOptions::new().read(true).write(true).open(&journal)?;
            let mut raw = Vec::new();
            f.read_to_end(&mut raw)?;
            let body = String::from_utf8(raw).map_err(|_| std::io::Error::other(Utf8Error))?;
            // text mode reads "\r" and "\r\n" as "\n"
            let ends = body.is_empty() || body.ends_with('\n') || body.ends_with('\r');
            let section = format!("{}{}\n", if ends { "" } else { "\n" }, lines.join("\n"));
            f.seek(SeekFrom::End(0))?;
            f.write_all(section.as_bytes())
        })();
        match res {
            Ok(()) => report.journal = Some(journal),
            Err(e) => {
                report.journal = None;
                let name = if e.get_ref().is_some_and(|inner| inner.is::<Utf8Error>()) {
                    "UnicodeDecodeError"
                } else {
                    tpy::os_error_name(&e)
                };
                warn!("dream: could not add the counts to the {day} journal ({name})");
            }
        }
    }

    // -- the loop ------------------------------------------------------------------------------

    /// `loop(shutdown, interval_secs=1800.0)`: wake soon after start, then every `interval_secs`, and dream
    /// whatever nights are due. Never fails. `shutdown` is the daemon's shutdown event: set once it holds true.
    pub async fn run_loop(&self, shutdown: watch::Receiver<bool>, interval_secs: f64) {
        self.run_loop_with(shutdown, interval_secs, |me, now| Ok(me.due_nights(now))).await
    }

    /// [`Dreamer::run_loop`] with the wake's choice of nights given (a test's seam where Python patched
    /// `due_nights`). An `Err` is a wake that raised: logged once a run, and the next wake tries again.
    pub async fn run_loop_with<F>(&self, mut shutdown: watch::Receiver<bool>, interval_secs: f64, mut due: F)
    where
        F: FnMut(&Dreamer, When) -> anyhow::Result<Vec<String>>,
    {
        let mut wait = FIRST_WAKE_SECS.min(interval_secs);
        while !is_set(&shutdown) {
            if wait_set(&mut shutdown, wait).await {
                return;
            }
            wait = interval_secs;
            match due(self, When::Aware((self.wall)())) {
                Ok(days) => {
                    for day in days {
                        if is_set(&shutdown) {
                            return;
                        }
                        self.dream(&day).await;
                    }
                }
                Err(e) => {
                    // the loop outlives any one wake
                    if !self.shared.loop_warned.swap(true, Ordering::SeqCst) {
                        warn!("dream: a wake failed ({}) — the next wake tries again", error_name(&e));
                    }
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the journal is not UTF-8")]
struct Utf8Error;

/// `folder.mkdir(mode=0o700, parents=True, exist_ok=True)`: the parents with the default mode, the folder 0o700.
fn make_dir(folder: &Path) -> std::io::Result<()> {
    if folder.is_dir() {
        return Ok(());
    }
    if let Some(parent) = folder.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    match DirBuilder::new().mode(0o700).create(folder) {
        Ok(()) => Ok(()),
        Err(_) if folder.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// `shutdown.is_set()`.
fn is_set(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

/// `await asyncio.wait_for(shutdown.wait(), timeout=secs)`: true when the event was set within `secs`. A sender
/// that is gone can never set it: that is a shutdown too (see PARITY.md, Differences).
async fn wait_set(shutdown: &mut watch::Receiver<bool>, secs: f64) -> bool {
    let set = shutdown.wait_for(|v| *v);
    match Duration::try_from_secs_f64(secs.max(0.0)) {
        Ok(d) => tokio::time::timeout(d, set).await.is_ok(),
        Err(_) => {
            let _ = set.await;
            true
        }
    }
}

#[cfg(test)]
mod tests;
