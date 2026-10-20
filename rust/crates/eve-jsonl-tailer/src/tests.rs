//! Ports of `bridge/tests/test_jsonl_tailer.py` and the tailer half of `bridge/tests/test_loop_offload.py`,
//! plus parity cases for behavior those files do not reach.

use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde_json::json;

// ---- helpers ---------------------------------------------------------------------------------------------------

fn noop_update() -> TokensCallback {
    Arc::new(|_, _, _, _, _| Box::pin(async { Ok(()) }))
}

type Emitted = Arc<Mutex<Vec<(String, String, String)>>>;

fn recording_text() -> (AssistantTextCallback, Emitted) {
    let emitted: Emitted = Arc::new(Mutex::new(Vec::new()));
    let sink = emitted.clone();
    let cb: AssistantTextCallback = Arc::new(move |p, t, u| {
        sink.lock().push((p, t, u));
        Box::pin(async { Ok(()) })
    });
    (cb, emitted)
}

fn tailer(root: &Path) -> JsonlTailer {
    JsonlTailer::new(noop_update(), Some(vec![root.to_path_buf()]), None).unwrap()
}

fn write_jsonl(path: &Path, records: &[Value]) {
    let body: Vec<String> = records.iter().map(|r| r.to_string()).collect();
    std::fs::write(path, body.join("\n") + "\n").unwrap();
}

fn append(path: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// Spin up a tailer, run the initial sweep, return it. No file watching.
async fn sync_sweep(root: &Path) -> JsonlTailer {
    let t = tailer(root);
    t.initial_sweep().await.unwrap();
    t
}

fn assistant(uuid: &str, text: &str) -> Value {
    json!({"uuid": uuid, "message": {"role": "assistant", "content": [
        {"type": "text", "text": text}], "usage": {"output_tokens": 1}}})
}

fn key(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

fn uuids(t: &JsonlTailer, p: &Path) -> HashSet<String> {
    t.state.lock().emitted_assistant_uuids[&key(p)].clone()
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// A change source that runs `before` on its first poll, then yields its batches, then ends.
struct Scripted {
    before: Option<Box<dyn FnOnce() + Send>>,
    batches: VecDeque<Result<HashSet<FileChange>, WatchError>>,
}

impl ChangeSource for Scripted {
    fn next_batch(&mut self) -> BoxFuture<'_, Option<Result<HashSet<FileChange>, WatchError>>> {
        if let Some(f) = self.before.take() {
            f();
        }
        let next = self.batches.pop_front();
        Box::pin(async move { next })
    }
}

fn scripted(
    before: Option<Box<dyn FnOnce() + Send>>,
    batches: Vec<Result<HashSet<FileChange>, WatchError>>,
) -> WatchFactory {
    let cell = Arc::new(Mutex::new(Some((before, batches))));
    Arc::new(move |_: &[PathBuf]| {
        let (before, batches) = cell.lock().take().unwrap_or((None, Vec::new()));
        Box::new(Scripted { before, batches: batches.into() }) as Box<dyn ChangeSource>
    })
}

fn no_watch() -> WatchFactory {
    scripted(None, Vec::new())
}

/// The real filesystem with hooks: count opens of one path, freeze sizes, slow every size() call down.
#[derive(Default)]
struct HookFs {
    opens: Mutex<Vec<String>>,
    fixed_size: Option<u64>,
    delay: Option<Duration>,
    size_calls: Mutex<Vec<(String, std::thread::ThreadId)>>,
}

impl TranscriptFs for HookFs {
    fn size(&self, path: &Path) -> io::Result<u64> {
        self.size_calls.lock().push((key(path), std::thread::current().id()));
        if let Some(d) = self.delay {
            std::thread::sleep(d);
        }
        match self.fixed_size {
            Some(s) => Ok(s),
            None => StdFs.size(path),
        }
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn ReadSeek>> {
        self.opens.lock().push(key(path));
        StdFs.open(path)
    }
}

// ---- test_jsonl_tailer.py: _record_is_today ----------------------------------------------------------------

fn utc_z(dt: chrono::DateTime<Utc>) -> PyValue {
    PyValue::Str(dt.to_rfc3339_opts(SecondsFormat::Micros, true))
}

#[test]
fn test_record_is_today_matches_local_day() {
    let ts = utc_z(Utc::now());
    assert!(record_is_today(Some(&ts), &today_key().unwrap()).unwrap());
}

#[test]
fn test_record_is_today_rejects_yesterday() {
    let ts = utc_z(Utc::now() - chrono::Duration::days(2));
    assert!(!record_is_today(Some(&ts), &today_key().unwrap()).unwrap());
}

#[test]
fn test_record_is_today_rejects_non_strings() {
    let today = today_key().unwrap();
    assert!(!record_is_today(None, &today).unwrap());
    assert!(!record_is_today(Some(&PyValue::None), &today).unwrap());
    assert!(!record_is_today(Some(&PyValue::Int(py::PyInt::small(12345))), &today).unwrap());
    assert!(!record_is_today(Some(&PyValue::Str(String::new())), &today).unwrap());
}

#[test]
fn test_record_is_today_rejects_bad_iso() {
    let today = today_key().unwrap();
    assert!(!record_is_today(Some(&PyValue::Str("not-a-date".into())), &today).unwrap());
    assert!(!record_is_today(Some(&PyValue::Str("2026/04/22".into())), &today).unwrap());
}

#[test]
fn test_record_is_today_handles_z_suffix() {
    // A timestamp fresh enough that it's definitely today in any tz.
    let ts = utc_z(Utc::now());
    assert!(matches!(&ts, PyValue::Str(s) if s.ends_with('Z')));
    assert!(record_is_today(Some(&ts), &today_key().unwrap()).unwrap());
}

// ---- test_jsonl_tailer.py: initial sweep -----------------------------------------------------------------------

/// Cost per file should be summed from usage records using the model's rates.
#[tokio::test]
async fn test_cost_accumulates_alongside_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let jsonl = dir.path().join("s.jsonl");
    write_jsonl(
        &jsonl,
        &[
            json!({"type": "assistant", "message": {
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{"type": "text", "text": "hi"}],
                "usage": {"input_tokens": 1_000_000, "output_tokens": 1_000_000},
            }}),
            json!({"type": "assistant", "message": {
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [{"type": "text", "text": "hi"}],
                "usage": {"output_tokens": 1_000_000},
            }}),
        ],
    );
    let t = sync_sweep(dir.path()).await;
    let st = t.state.lock();
    // Sonnet (3 + 15) + Opus (75) = $93
    assert!((st.cost_per_file[&key(&jsonl)] - 93.0).abs() < 1e-6);
    // Both records lack a timestamp, so today's map stays empty.
    assert_eq!(st.today_cost_per_file.get(&key(&jsonl)).copied().unwrap_or(0.0), 0.0);
}

/// An assistant record without a content array shouldn't crash; its tokens still count.
#[tokio::test]
async fn test_assistant_record_without_content_does_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let jsonl = dir.path().join("s.jsonl");
    write_jsonl(
        &jsonl,
        &[
            json!({"type": "assistant", "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "first"}],
                "usage": {"output_tokens": 1},
            }}),
            json!({"type": "assistant", "message": {
                "role": "assistant",
                "usage": {"output_tokens": 1},
            }}),
        ],
    );
    let t = sync_sweep(dir.path()).await;
    assert_eq!(t.state.lock().tokens_per_file[&key(&jsonl)], 2);
}

#[tokio::test]
async fn test_run_reads_history_once_and_does_not_replay_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(&path, &[assistant("old", "hello")]);
    let fs = Arc::new(HookFs::default());
    let (on_text, emitted) = recording_text();
    let t = JsonlTailer::new(noop_update(), Some(vec![dir.path().to_path_buf()]), Some(on_text))
        .unwrap()
        .with_fs(fs.clone())
        .with_watch_factory(no_watch());
    t.run().await.unwrap();
    let reads: Vec<String> = fs.opens.lock().iter().filter(|p| **p == key(&path)).cloned().collect();
    assert_eq!(reads, vec![key(&path)]);
    assert_eq!(uuids(&t, &path), set(&["old"]));
    assert!(emitted.lock().is_empty());
}

#[tokio::test]
async fn test_record_appended_after_history_sweep_is_emitted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(&path, &[assistant("old", "hello")]);
    let (on_text, emitted) = recording_text();
    let appended = path.clone();
    let factory = scripted(
        Some(Box::new(move || append(&appended, &(assistant("new", "new answer").to_string() + "\n")))),
        vec![Ok([(Change::Modified, key(&path))].into_iter().collect())],
    );
    let t = JsonlTailer::new(noop_update(), Some(vec![dir.path().to_path_buf()]), Some(on_text))
        .unwrap()
        .with_watch_factory(factory);
    t.run().await.unwrap();
    assert_eq!(*emitted.lock(), vec![(key(&path), "new answer".to_string(), "new".to_string())]);
    assert_eq!(t.state.lock().tokens_per_file[&key(&path)], 2);
}

#[tokio::test]
async fn test_non_object_record_does_not_discard_later_valid_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    std::fs::write(&path, format!("null\n[]\n42\n{}\n", assistant("a1", "hello"))).unwrap();
    let t = sync_sweep(dir.path()).await;
    assert_eq!(t.state.lock().tokens_per_file[&key(&path)], 1);
}

#[tokio::test]
async fn test_deleting_transcript_releases_assistant_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(&path, &[assistant("old", "hello")]);
    let t = sync_sweep(dir.path()).await;
    t.state.lock().emitted_assistant_uuids.insert(key(&path), set(&["old"]));
    assert!(t.state.lock().emitted_assistant_uuids.contains_key(&key(&path)));
    t.handle_changes([(Change::Deleted, key(&path))].into_iter().collect()).await.unwrap();
    let st = t.state.lock();
    assert!(!st.emitted_assistant_uuids.contains_key(&key(&path)));
    assert!(!st.offsets.contains_key(&key(&path)));
    assert!(!st.tokens_per_file.contains_key(&key(&path)));
    assert!(!st.cost_per_file.contains_key(&key(&path)));
}

#[tokio::test]
async fn test_partial_line_is_left_for_the_next_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let complete = assistant("old", "hello").to_string() + "\n";
    let partial = assistant("new", "new text").to_string();
    std::fs::write(&path, format!("{complete}{partial}")).unwrap();
    let t = sync_sweep(dir.path()).await;
    let k = key(&path);
    {
        let mut st = t.state.lock();
        assert_eq!(st.offsets[&k], complete.len() as u64);
        assert_eq!(st.emitted_assistant_uuids[&k], set(&["old"]));
        st.initial_sweep_done = true;
        st.has_text_callback = true;
    }
    append(&path, "\n");
    let mut st = t.state.lock();
    st.process_file(&k).unwrap();
    assert_eq!(st.tokens_per_file[&k], 2);
    assert_eq!(st.pending_assistant_emits, vec![(k.clone(), "new text".to_string(), "new".to_string())]);
    st.process_file(&k).unwrap();
    assert_eq!(st.tokens_per_file[&k], 2);
    assert_eq!(st.pending_assistant_emits.len(), 1);
}

#[tokio::test]
async fn test_truncated_transcript_resets_counters_and_partial_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(&path, &[assistant("old", "hello"), assistant("older", "hello")]);
    let t = sync_sweep(dir.path()).await;
    let k = key(&path);
    assert_eq!(t.state.lock().tokens_per_file[&k], 2);
    let text = assistant("new", "hello").to_string();
    std::fs::write(&path, &text[..text.len() - 1]).unwrap();
    let mut st = t.state.lock();
    st.process_file(&k).unwrap();
    assert_eq!(st.tokens_per_file.get(&k).copied().unwrap_or(0), 0);
    append(&path, "}\n");
    st.process_file(&k).unwrap();
    assert_eq!(st.tokens_per_file[&k], 1);
    assert_eq!(st.offsets[&k], std::fs::metadata(&path).unwrap().len());
}

#[tokio::test]
async fn test_process_file_stops_at_its_size_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(&path, &[assistant("first", "hello")]);
    let snapshot_size = std::fs::metadata(&path).unwrap().len();
    append(&path, &(assistant("later", "hello").to_string() + "\n"));
    let fs = Arc::new(HookFs { fixed_size: Some(snapshot_size), ..Default::default() });
    let t = tailer(dir.path()).with_fs(fs);
    t.initial_sweep().await.unwrap();
    let st = t.state.lock();
    let k = key(&path);
    assert_eq!(st.tokens_per_file[&k], 1);
    assert_eq!(st.offsets[&k], snapshot_size);
    assert_eq!(st.emitted_assistant_uuids[&k], set(&["first"]));
}

// tracemalloc's stand-in: a global allocator that tracks live bytes and their peak for the current thread only,
// so tests running in parallel do not disturb the measurement.
struct TrackingAlloc;

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = TRACKING.try_with(|on| {
        if on.get() {
            let _ = LIVE.try_with(|live| {
                let now = live.get() + delta;
                live.set(now);
                let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
            });
        }
    });
}

unsafe impl GlobalAlloc for TrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            track(layout.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: TrackingAlloc = TrackingAlloc;

/// `tracemalloc.start()` .. `get_traced_memory()[1]` around `f`, on this thread.
fn peak_of<R>(f: impl FnOnce() -> R) -> (R, isize) {
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    TRACKING.with(|t| t.set(true));
    let r = f();
    TRACKING.with(|t| t.set(false));
    (r, PEAK.with(|p| p.get()))
}

#[test]
fn test_process_file_memory_does_not_scale_with_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (name, count) in [("small", 400usize), ("large", 4000usize)] {
        let path = dir.path().join(format!("{name}.jsonl"));
        let mut f = std::fs::File::create(&path).unwrap();
        let line = json!({"message": {"role": "assistant", "content": "x".repeat(200), "usage": {"output_tokens": 1}}})
            .to_string()
            + "\n";
        for _ in 0..count {
            f.write_all(line.as_bytes()).unwrap();
        }
        paths.push((path, count));
    }

    let mut peaks = Vec::new();
    for (path, count) in &paths {
        let t = tailer(dir.path());
        let mut st = t.state.lock();
        let k = key(path);
        let (r, peak) = peak_of(|| st.process_file(&k));
        r.unwrap();
        peaks.push(peak);
        assert_eq!(st.tokens_per_file[&k], *count as i64);
    }
    assert!(peaks[1] < 2 * peaks[0] + 100_000, "{peaks:?}");
    // Positive control: the previous bulk-read/split approach holds all lines.
    let (lines, bulk_peak) = peak_of(|| {
        let raw = std::fs::read(&paths[1].0).unwrap();
        let lines: Vec<Vec<u8>> = raw.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| l.to_vec()).collect();
        lines.len()
    });
    assert_eq!(lines, 4000);
    assert!(bulk_peak > 5 * peaks[1], "{peaks:?} {bulk_peak}");
}

#[tokio::test]
async fn test_malformed_usage_does_not_discard_later_valid_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let mut malformed: Vec<Value> = (0..3).map(|i| assistant(&format!("bad-{i}"), "hello")).collect();
    malformed[0]["message"]["usage"]["output_tokens"] = json!("not a count");
    malformed[1]["message"]["usage"]["input_tokens"] = json!([]);
    malformed[1]["message"]["usage"]["cache_creation"] = json!([1]);
    malformed[2]["message"]["model"] = json!({"not": "a model id"});
    malformed.push(assistant("valid", "hello"));
    write_jsonl(&path, &malformed);
    let t = sync_sweep(dir.path()).await;
    let st = t.state.lock();
    assert_eq!(st.tokens_per_file[&key(&path)], 1);
    assert_eq!(st.offsets[&key(&path)], std::fs::metadata(&path).unwrap().len());
}

// ---- test_loop_offload.py (the tailer tests) -------------------------------------------------------------------

/// How many times the loop came round while `fut` ran. Run on a current-thread runtime: work that blocks the
/// loop stops the ticker.
async fn ticks_during<F: std::future::Future>(fut: F) -> (F::Output, usize) {
    let done = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticker = {
        let (done, ticks) = (done.clone(), ticks.clone());
        tokio::spawn(async move {
            while !done.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
                ticks.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    let out = fut.await;
    done.store(true, Ordering::SeqCst);
    ticker.await.unwrap();
    (out, ticks.load(Ordering::SeqCst))
}

#[tokio::test(flavor = "current_thread")]
async fn test_initial_sweep_keeps_the_loop_ticking() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..3 {
        std::fs::write(dir.path().join(format!("s{i}.jsonl")), json!({"type": "user", "uuid": format!("u{i}")}).to_string() + "\n")
            .unwrap();
    }
    // A transcript that takes 0.2 s to read.
    let fs = Arc::new(HookFs { delay: Some(Duration::from_millis(200)), ..Default::default() });
    let t = tailer(dir.path()).with_fs(fs.clone());
    let (r, ticks) = ticks_during(t.initial_sweep()).await;
    r.unwrap();
    assert_eq!(fs.size_calls.lock().len(), 3); // the sweep still visited every file
    assert!(ticks >= 10, "{ticks}"); // ~0.6 s of work: a blocked loop would show ~0
}

/// The real `run()` wiring: history and UUIDs are processed together off the loop.
#[tokio::test(flavor = "current_thread")]
async fn test_run_keeps_the_loop_ticking_through_history() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("s.jsonl");
    std::fs::write(&p, json!({"message": {"role": "assistant"}, "uuid": "a1"}).to_string() + "\n").unwrap();
    // A year of transcripts takes a while to read.
    let fs = Arc::new(HookFs { delay: Some(Duration::from_millis(300)), ..Default::default() });
    let t = tailer(dir.path()).with_fs(fs).with_watch_factory(no_watch());
    let (r, ticks) = ticks_during(t.run()).await;
    r.unwrap();
    assert!(ticks >= 5, "{ticks}");
    assert!(uuids(&t, &p).contains("a1"));
    assert!(t.state.lock().initial_sweep_done);
}

/// A newly appended large record must not pause wake-word or BLE work on the loop.
#[tokio::test(flavor = "current_thread")]
async fn test_live_transcript_changes_keep_the_loop_ticking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    std::fs::write(&path, json!({"message": {"role": "assistant", "usage": {"output_tokens": 3}}}).to_string() + "\n").unwrap();
    let fs = Arc::new(HookFs { delay: Some(Duration::from_millis(200)), ..Default::default() });
    let t = tailer(dir.path()).with_fs(fs.clone());
    let caller_thread = std::thread::current().id();
    let (r, ticks) = ticks_during(t.handle_changes([(Change::Modified, key(&path))].into_iter().collect())).await;
    r.unwrap();
    assert!(ticks >= 5, "{ticks}");
    let threads: Vec<_> = fs.size_calls.lock().iter().map(|(_, id)| *id).collect();
    assert!(!threads.is_empty() && threads.iter().all(|id| *id != caller_thread));
    assert_eq!(t.state.lock().tokens_per_file[&key(&path)], 3);
}

// ---- parity cases beyond the Python tests ----------------------------------------------------------------------

/// A fixed UTC offset standing in for `localtime()`.
struct FixedZone(i64);

impl LocalZone for FixedZone {
    fn utc_offset(&self, _timestamp: i64) -> Option<i64> {
        Some(self.0)
    }
}

#[tokio::test]
async fn today_counters_follow_the_record_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let old = (Utc::now() - chrono::Duration::days(3)).to_rfc3339_opts(SecondsFormat::Millis, true);
    write_jsonl(
        &path,
        &[
            json!({"timestamp": now, "message": {"role": "assistant", "model": "claude-haiku-4-5", "usage": {"output_tokens": 1_000_000}}}),
            json!({"timestamp": old, "message": {"role": "assistant", "usage": {"output_tokens": 5}}}),
            json!({"timestamp": 17, "message": {"role": "assistant", "usage": {"output_tokens": 7}}}),
        ],
    );
    let t = sync_sweep(dir.path()).await;
    let (cum, today, cost_cum, cost_today) = t.totals();
    assert_eq!((cum, today), (1_000_012, 1_000_000));
    assert!((cost_today - 5.0).abs() < 1e-9, "{cost_today}");
    assert!(cost_cum > cost_today);
}

#[tokio::test]
async fn usage_read_like_python_int() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let lines = [
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": "12"}}}"#, // int("12")
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": 2.9}}}"#,  // int(2.9) == 2
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": true}}}"#, // int(True) == 1
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": 0.5}}}"#,  // int(0.5) == 0: skipped
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": NaN}}}"#,  // ValueError
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": 1e400}}}"#, // OverflowError
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": 1, "cache_creation": "x"}}}"#, // AttributeError
        r#"{"message": {"role": "assistant", "model": 5, "usage": {"output_tokens": 1}}}"#, // AttributeError
        r#"{"message": {"role": "assistant", "model": 0, "usage": {"output_tokens": 100}}}"#, // `0 or ""`
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": 1, "cache_creation": {"ephemeral_5m_input_tokens": 1}, "cache_creation_input_tokens": "bad"}}}"#, // never read
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": 1, "cache_creation_input_tokens": "bad"}}}"#, // ValueError
        r#"{"message": {"role": "assistant", "usage": {"output_tokens": "-3"}}}"#, // negative counts count
        r#"{"usage": {"output_tokens": 1000}}"#, // a top-level usage with no message
        r#"{"message": "not a dict", "usage": {"output_tokens": 10000}}"#, // falls back to the record itself
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let t = sync_sweep(dir.path()).await;
    assert_eq!(t.state.lock().tokens_per_file[&key(&path)], 12 + 2 + 1 + 100 + 1 - 3 + 1000 + 10000);
}

#[tokio::test]
async fn live_text_emits_first_text_block_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    std::fs::write(&path, "").unwrap();
    let (on_text, emitted) = recording_text();
    let t = JsonlTailer::new(noop_update(), Some(vec![dir.path().to_path_buf()]), Some(on_text)).unwrap();
    t.initial_sweep().await.unwrap();
    t.state.lock().initial_sweep_done = true;
    let records = [
        json!({"uuid": "t1", "message": {"role": "assistant", "content": [{"type": "tool_use", "name": "x"}]}}),
        json!({"uuid": "t2", "message": {"role": "assistant", "content": [
            {"type": "text", "text": "   "}, {"type": "text", "text": 5}, {"type": "text", "text": "\u{2000} first \n"},
            {"type": "text", "text": "second"}]}}),
        json!({"uuid": "t2", "message": {"role": "assistant", "content": [{"type": "text", "text": "again"}]}}),
        json!({"uuid": "", "message": {"role": "assistant", "content": [{"type": "text", "text": "no uuid"}]}}),
        json!({"uuid": "u1", "message": {"role": "user", "content": [{"type": "text", "text": "user"}]}}),
        json!({"uuid": "t3", "message": {"role": "assistant", "content": "plain string"}}),
    ];
    let body: String = records.iter().map(|r| r.to_string() + "\n").collect();
    append(&path, &body);
    t.handle_changes([(Change::Modified, key(&path))].into_iter().collect()).await.unwrap();
    t.fire_pending_emits().await;
    assert_eq!(*emitted.lock(), vec![(key(&path), "first".to_string(), "t2".to_string())]);
    // A uuid key appears for every assistant record with a uuid, even ones that emitted nothing.
    assert_eq!(uuids(&t, &path), set(&["t2"]));
    assert!(t.state.lock().pending_assistant_emits.is_empty());
}

#[tokio::test]
async fn failing_text_callback_does_not_stop_the_rest() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let cb: AssistantTextCallback = Arc::new(move |_, _, _| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { if n == 0 { Err(anyhow::anyhow!("boom")) } else { Ok(()) } })
    });
    let dir = tempfile::tempdir().unwrap();
    let t = JsonlTailer::new(noop_update(), Some(vec![dir.path().to_path_buf()]), Some(cb)).unwrap();
    t.state.lock().pending_assistant_emits =
        vec![("a".into(), "1".into(), "x".into()), ("b".into(), "2".into(), "y".into())];
    t.fire_pending_emits().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_astimezone_error_ends_the_file_pass_after_counting_the_record() {
    // Python's astimezone() raises for an instant whose local time leaves year 1..9999, and _record_is_today
    // does not catch it: the cumulative counter already has the record, today's does not, and the rest of the
    // file waits for the next change.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(
        &path,
        &[
            json!({"timestamp": "0001-01-01T00:00:00+05:00", "message": {"role": "assistant", "usage": {"output_tokens": 2}}}),
            json!({"message": {"role": "assistant", "usage": {"output_tokens": 3}}}),
        ],
    );
    let t = tailer(dir.path()).with_zone(Arc::new(FixedZone(-5 * 3600)));
    let k = key(&path);
    let first_len = std::fs::read_to_string(&path).unwrap().lines().next().unwrap().len() as u64 + 1;
    let mut st = t.state.lock();
    assert!(matches!(st.process_file(&k), Err(ProcessError::Time(_))));
    assert_eq!(st.tokens_per_file[&k], 2);
    assert!(!st.today_tokens_per_file.contains_key(&k));
    assert_eq!(st.offsets[&k], first_len);
    st.process_file(&k).unwrap();
    assert_eq!(st.tokens_per_file[&k], 5);
}

#[tokio::test]
async fn deep_nesting_ends_the_file_pass_like_a_recursion_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let deep = "[".repeat(py::MAX_JSON_DEPTH + 1) + &"]".repeat(py::MAX_JSON_DEPTH + 1);
    std::fs::write(&path, format!("{deep}\n{}\n", json!({"message": {"usage": {"output_tokens": 4}}}))).unwrap();
    let t = tailer(dir.path());
    let k = key(&path);
    let mut st = t.state.lock();
    assert!(matches!(st.process_file(&k), Err(ProcessError::Recursion)));
    assert!(!st.tokens_per_file.contains_key(&k));
    st.process_file(&k).unwrap();
    assert_eq!(st.tokens_per_file[&k], 4);
}

#[tokio::test]
async fn lone_surrogate_escape_does_not_lose_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    std::fs::write(
        &path,
        "{\"message\": {\"role\": \"assistant\", \"content\": [{\"type\": \"text\", \"text\": \"cut \\ud83d\"}], \"usage\": {\"output_tokens\": 9}}}\n",
    )
    .unwrap();
    let t = sync_sweep(dir.path()).await;
    assert_eq!(t.state.lock().tokens_per_file[&key(&path)], 9);
}

#[tokio::test]
async fn day_rollover_clears_today_counters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_jsonl(&path, &[json!({"message": {"usage": {"output_tokens": 1}}})]);
    let t = tailer(dir.path());
    let k = key(&path);
    let mut st = t.state.lock();
    st.day_key = "1999-01-01".into();
    st.today_tokens_per_file.insert("other".into(), 50);
    st.today_cost_per_file.insert("other".into(), 1.0);
    st.process_file(&k).unwrap();
    assert_eq!(st.day_key, today_key().unwrap());
    assert!(st.today_tokens_per_file.is_empty() && st.today_cost_per_file.is_empty());
    // Nothing new to read: the day check does not run.
    st.day_key = "1999-01-01".into();
    st.today_tokens_per_file.insert("other".into(), 50);
    st.process_file(&k).unwrap();
    assert_eq!(st.today_tokens_per_file["other"], 50);
}

#[tokio::test]
async fn blank_and_bad_lines_are_consumed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let body: &[u8] = b"\n   \n\x0b\x0c\n{not json\n\xef\xbb\xbf{\"usage\": {\"output_tokens\": 6}}\n";
    std::fs::write(&path, body).unwrap();
    let t = sync_sweep(dir.path()).await;
    let st = t.state.lock();
    assert_eq!(st.tokens_per_file[&key(&path)], 6);
    assert_eq!(st.offsets[&key(&path)], body.len() as u64);
}

#[tokio::test]
async fn non_jsonl_changes_are_ignored_and_missing_files_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, "{\"usage\": {\"output_tokens\": 6}}\n").unwrap();
    let t = tailer(dir.path());
    let gone = key(&dir.path().join("gone.jsonl"));
    t.handle_changes([(Change::Modified, key(&txt)), (Change::Added, gone.clone())].into_iter().collect())
        .await
        .unwrap();
    let st = t.state.lock();
    assert!(st.tokens_per_file.is_empty() && st.offsets.is_empty());
}

#[test]
fn rglob_walks_like_pathlib() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("a/deep")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::create_dir_all(root.join("dir.jsonl")).unwrap();
    for f in ["top.jsonl", "a/one.jsonl", "a/deep/two.jsonl", "b/three.jsonl", ".jsonl", "a/skip.json", "a/X.JSONL"] {
        std::fs::write(root.join(f), "").unwrap();
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("top.jsonl"), root.join("alias.jsonl")).unwrap();
    }
    let mut seen = Vec::new();
    rglob_jsonl(root, &mut |p: &Path| seen.push(p.strip_prefix(root).unwrap().to_str().unwrap().to_string()));
    let mut sorted = seen.clone();
    sorted.sort();
    let mut want = vec!["top.jsonl", ".jsonl", "dir.jsonl", "a/one.jsonl", "a/deep/two.jsonl", "b/three.jsonl"];
    if cfg!(unix) {
        want.push("alias.jsonl");
    }
    want.sort();
    assert_eq!(sorted, want);
    // The root's own matches come first, and a directory's matches before anything below it.
    let pos = |s: &str| seen.iter().position(|x| x == s).unwrap();
    assert!(pos("top.jsonl") < pos("a/one.jsonl") && pos("top.jsonl") < pos("b/three.jsonl"));
    assert!(pos("a/one.jsonl") < pos("a/deep/two.jsonl"));
}

#[tokio::test]
async fn run_creates_missing_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects/nested");
    let t = JsonlTailer::new(noop_update(), Some(vec![root.clone()]), None).unwrap().with_watch_factory(no_watch());
    t.run().await.unwrap();
    assert!(root.is_dir());
}

#[tokio::test]
async fn run_reports_totals_and_survives_a_watch_error() {
    let dir = tempfile::tempdir().unwrap();
    write_jsonl(&dir.path().join("s.jsonl"), &[json!({"message": {"usage": {"output_tokens": 4}}})]);
    let updates: Arc<Mutex<Vec<(i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let u = updates.clone();
    let on_update: TokensCallback = Arc::new(move |c, t, _, _, e| {
        assert!(e.is_empty());
        u.lock().push((c, t));
        Box::pin(async { Ok(()) })
    });
    let factory = scripted(
        None,
        vec![Ok([(Change::Modified, "/elsewhere/x.txt".to_string())].into_iter().collect()), Err(WatchError::Internal("boom".into()))],
    );
    let t = JsonlTailer::new(on_update, Some(vec![dir.path().to_path_buf()]), None).unwrap().with_watch_factory(factory);
    t.run().await.unwrap();
    // Once after the sweep, once after the first batch; the error ends the loop quietly.
    assert_eq!(*updates.lock(), vec![(4, 0), (4, 0)]);
}

#[tokio::test]
async fn initial_update_error_propagates_but_later_ones_end_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let fail_first: TokensCallback = Arc::new(|_, _, _, _, _| Box::pin(async { Err(anyhow::anyhow!("first")) }));
    let t = JsonlTailer::new(fail_first, Some(vec![dir.path().to_path_buf()]), None).unwrap().with_watch_factory(no_watch());
    assert!(t.run().await.is_err());

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let fail_second: TokensCallback = Arc::new(move |_, _, _, _, _| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { if n == 0 { Ok(()) } else { Err(anyhow::anyhow!("later")) } })
    });
    let batch = || Ok([(Change::Modified, "/x/y.txt".to_string())].into_iter().collect());
    let t = JsonlTailer::new(fail_second, Some(vec![dir.path().to_path_buf()]), None)
        .unwrap()
        .with_watch_factory(scripted(None, vec![batch(), batch(), batch()]));
    t.run().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn cost_totals_add_like_python_sum() {
    let dir = tempfile::tempdir().unwrap();
    let t = tailer(dir.path());
    let mut st = t.state.lock();
    for (i, c) in [0.1, 0.2, 0.3, 1e16, -1e16].iter().enumerate() {
        st.cost_per_file.insert(format!("f{i}"), *c);
    }
    // Python 3.13: sum([0.1, 0.2, 0.3, 1e16, -1e16]) == 0.6; plain left-to-right addition gives 0.0.
    assert_eq!(st.totals().2, 0.6);
    assert_eq!([0.1, 0.2, 0.3, 1e16, -1e16].iter().sum::<f64>(), 0.0);
}

#[test]
fn new_defaults_to_transcript_roots() {
    let t = JsonlTailer::new(noop_update(), Some(Vec::new()), None).unwrap();
    assert_eq!(t.roots(), eve_claude_home::transcript_roots(None).unwrap().as_slice());
}
