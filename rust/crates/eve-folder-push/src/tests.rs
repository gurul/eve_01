//! Ports of bridge/tests/test_folder_push.py and the folder-push half (C) of
//! bridge/tests/test_serial_lean.py, plus parity checks for the error paths.

use super::*;
use std::path::Path;
use std::sync::Arc;

// ---- stub daemon (test_folder_push.StubDaemon) -------------------------------

/// Records everything written and answers each wait_for_ack with ok=True.
#[derive(Default)]
struct StubDaemon {
    sent: Mutex<Vec<Value>>,
    /// Answer every ack with this instead of ok=True, when set.
    answer: Option<Value>,
    /// Make every BLE write fail.
    fail_writes: bool,
}

impl StubDaemon {
    fn chunk_count(&self) -> usize {
        self.sent
            .lock()
            .iter()
            .filter(|m| m.get("cmd").and_then(Value::as_str) == Some("chunk"))
            .count()
    }

    fn cmds(&self) -> Vec<String> {
        self.sent
            .lock()
            .iter()
            .map(|m| m.get("cmd").and_then(Value::as_str).unwrap_or("").to_string())
            .collect()
    }
}

#[async_trait]
impl PushTarget for StubDaemon {
    type Waiter = ();

    fn expect_ack(&self, _ack_type: &str, _n: Option<i64>) -> Self::Waiter {}

    async fn send(&self, payload: &Value) -> bool {
        if self.fail_writes {
            return false;
        }
        self.sent.lock().push(payload.clone());
        true
    }

    async fn wait_for_ack(
        &self,
        ack_type: &str,
        _timeout: Duration,
        _waiter: Self::Waiter,
    ) -> Result<Value, AckWaitError> {
        if let Some(a) = &self.answer {
            return Ok(a.clone());
        }
        // Fulfil the protocol: for chunk acks we return a running byte counter.
        if ack_type == "chunk" {
            return Ok(json!({"ack": "chunk", "ok": true, "n": self.chunk_count() * CHUNK_SIZE}));
        }
        Ok(json!({"ack": ack_type, "ok": true}))
    }
}

// ---- helpers -------------------------------------------------------------

fn make_pack(tmp: &Path, files: &[(&str, &[u8])], manifest_name: Option<&str>) -> PathBuf {
    let folder = tmp.join("pack");
    std::fs::create_dir(&folder).unwrap();
    if let Some(name) = manifest_name {
        // json.dumps({"name": name})
        std::fs::write(folder.join("manifest.json"), format!("{{\"name\": \"{name}\"}}")).unwrap();
    }
    for (name, data) in files {
        std::fs::write(folder.join(name), data).unwrap();
    }
    folder
}

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files.iter().map(|p| path_name(p)).collect()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

// ---- unit: enumerate / name resolution -----------------------------------

#[test]
fn test_enumerate_skips_dotfiles() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("a.gif", b"x"), (".hidden", b"y")], None);
    let files = enumerate_files(&folder).unwrap();
    assert_eq!(names(&files), vec!["a.gif"]);
}

#[test]
fn test_enumerate_puts_manifest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("z.gif", b"x"), ("b.gif", b"y")], Some("pet"));
    let files = enumerate_files(&folder).unwrap();
    assert_eq!(path_name(&files[0]), "manifest.json");
    assert_eq!(names(&files[1..]), vec!["b.gif", "z.gif"]);
}

#[test]
fn enumerate_skips_subdirectories_and_follows_file_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("b.gif", b"y")], None);
    std::fs::create_dir(folder.join("sub")).unwrap();
    std::fs::write(folder.join("sub").join("inner.gif"), b"z").unwrap();
    let outside = tmp.path().join("outside.gif");
    std::fs::write(&outside, b"o").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("a.gif")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("missing"), folder.join("c.gif")).unwrap();
    let files = enumerate_files(&folder).unwrap();
    assert_eq!(names(&files), vec!["a.gif", "b.gif"]);
}

#[test]
fn enumerate_orders_by_code_point() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(
        tmp.path(),
        &[("b.gif", b"1"), ("C.gif", b"2"), ("é.gif", b"3"), ("a.gif", b"4")],
        Some("x"),
    );
    let files = enumerate_files(&folder).unwrap();
    assert_eq!(
        names(&files),
        vec!["manifest.json", "C.gif", "a.gif", "b.gif", "é.gif"]
    );
}

#[test]
fn test_pack_name_prefers_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("x.gif", b"x")], Some("bufo"));
    assert_eq!(pack_name(&folder).unwrap(), "bufo");
}

#[test]
fn test_pack_name_falls_back_to_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("x.gif", b"x")], None);
    assert_eq!(pack_name(&folder).unwrap(), "pack");
}

#[test]
fn test_pack_name_ignores_bad_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("weird");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("manifest.json"), "not json at all").unwrap();
    std::fs::write(folder.join("x.gif"), b"x").unwrap();
    assert_eq!(pack_name(&folder).unwrap(), "weird");
}

#[test]
fn pack_name_strips_and_rejects_blank_or_non_string_names() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("p");
    std::fs::create_dir(&folder).unwrap();
    let m = folder.join("manifest.json");
    std::fs::write(&m, r#"{"name": "  bufo\u001f\n"}"#).unwrap();
    assert_eq!(pack_name(&folder).unwrap(), "bufo");
    std::fs::write(&m, r#"{"name": "   "}"#).unwrap();
    assert_eq!(pack_name(&folder).unwrap(), "p");
    std::fs::write(&m, r#"{"name": 7}"#).unwrap();
    assert_eq!(pack_name(&folder).unwrap(), "p");
    std::fs::write(&m, b"\xff\xfe{}").unwrap();
    assert_eq!(pack_name(&folder).unwrap(), "p");
    // A directory named manifest.json: read_text raises OSError → folder name.
    std::fs::remove_file(&m).unwrap();
    std::fs::create_dir(&m).unwrap();
    assert_eq!(pack_name(&folder).unwrap(), "p");
}

#[test]
fn pack_name_non_object_manifest_raises_attribute_error() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("p");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("manifest.json"), "[1, 2]").unwrap();
    let err = pack_name(&folder).unwrap_err();
    assert_eq!(err.type_name(), "AttributeError");
    assert_eq!(err.to_string(), "'list' object has no attribute 'get'");
}

// ---- integration: full protocol sequence --------------------------------

#[test]
fn test_push_character_sends_canonical_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let data_a = vec![b'a'; CHUNK_SIZE + 50]; // 2 chunks
    let data_b = vec![b'b'; 10]; // 1 chunk
    let folder = make_pack(tmp.path(), &[("a.gif", &data_a), ("b.gif", &data_b)], Some("pet"));

    let daemon = StubDaemon::default();
    let result = run(push_character(&daemon, s(&folder))).unwrap();

    assert_eq!(result.name, "pet");
    assert_eq!(result.files, 3); // manifest + a + b
    assert_eq!(
        result.total_bytes as usize,
        data_a.len() + data_b.len() + br#"{"name": "pet"}"#.len()
    );

    let cmds = daemon.cmds();
    // Starts with char_begin, ends with char_end
    assert_eq!(cmds[0], "char_begin");
    assert_eq!(cmds.last().unwrap(), "char_end");
    // Three file/file_end bookends
    assert_eq!(cmds.iter().filter(|c| *c == "file").count(), 3);
    assert_eq!(cmds.iter().filter(|c| *c == "file_end").count(), 3);
    // Chunks for the two real gifs: 2 for a, 1 for b, plus some for manifest
    assert!(cmds.iter().filter(|c| *c == "chunk").count() >= 3);
}

#[test]
fn push_character_wire_payloads_match_python() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("a.gif", b"hi")], None);
    let daemon = StubDaemon::default();
    let out = run(push_character(&daemon, s(&folder))).unwrap();
    let sent: Vec<String> = daemon.sent.lock().iter().map(|v| v.to_string()).collect();
    assert_eq!(
        sent,
        vec![
            r#"{"cmd":"char_begin","name":"pack","total":2}"#,
            r#"{"cmd":"file","path":"a.gif","size":2}"#,
            r#"{"cmd":"chunk","d":"aGk="}"#,
            r#"{"cmd":"file_end"}"#,
            r#"{"cmd":"char_end"}"#,
        ]
    );
    assert_eq!(
        out.to_json().to_string(),
        r#"{"name":"pack","files":1,"total_bytes":2}"#
    );
}

#[test]
fn test_push_character_chunks_are_valid_base64() {
    let tmp = tempfile::tempdir().unwrap();
    let original: Vec<u8> = [0u8, 1, 2, 0xff].repeat(200);
    let folder = make_pack(tmp.path(), &[("raw.bin", &original)], Some("p"));
    let daemon = StubDaemon::default();
    run(push_character(&daemon, s(&folder))).unwrap();
    // Concatenate all chunks for raw.bin — they should base64-decode back to original
    let mut decoded = Vec::new();
    let mut collecting = false;
    let mut sizes = Vec::new();
    for m in daemon.sent.lock().iter() {
        let cmd = m.get("cmd").and_then(Value::as_str);
        if cmd == Some("file") && m.get("path").and_then(Value::as_str) == Some("raw.bin") {
            collecting = true;
            continue;
        }
        if cmd == Some("file_end") && collecting {
            break;
        }
        if collecting && cmd == Some("chunk") {
            let piece = base64::engine::general_purpose::STANDARD
                .decode(m["d"].as_str().unwrap())
                .unwrap();
            sizes.push(piece.len());
            decoded.extend(piece);
        }
    }
    assert_eq!(decoded, original);
    assert_eq!(sizes, vec![200, 200, 200, 200]);
}

#[test]
fn test_push_character_rejects_oversize() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("big");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("huge.gif"), vec![b'x'; MAX_TOTAL_BYTES as usize + 1]).unwrap();
    let daemon = StubDaemon::default();
    let err = run(push_character(&daemon, s(&folder))).unwrap_err();
    assert_eq!(err.type_name(), "ValueError");
    assert!(err.to_string().contains("too large"));
    assert_eq!(
        err.to_string(),
        "folder too large: 1,800,001 bytes > 1,800,000 cap"
    );
    assert!(daemon.sent.lock().is_empty());
}

#[test]
fn test_push_character_rejects_non_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = StubDaemon::default();
    let err = run(push_character(&daemon, s(&tmp.path().join("nope")))).unwrap_err();
    assert_eq!(err.type_name(), "ValueError");
    assert!(err.to_string().contains("not a directory"));
    let real = std::fs::canonicalize(tmp.path()).unwrap();
    assert_eq!(
        err.to_string(),
        format!("not a directory: {}", real.join("nope").display())
    );
}

#[test]
fn test_push_character_rejects_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("empty");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join(".DS_Store"), b"x").unwrap();
    let err = run(push_character(&StubDaemon::default(), s(&folder))).unwrap_err();
    assert_eq!(err.type_name(), "ValueError");
    assert!(err.to_string().contains("no files"));
}

#[test]
fn push_character_rejected_ack_raises_runtime_error() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("a.gif", b"x")], None);
    let daemon = StubDaemon {
        answer: Some(json!({"ack": "char_begin", "ok": false, "error": "fs full"})),
        ..Default::default()
    };
    let err = run(push_character(&daemon, s(&folder))).unwrap_err();
    assert_eq!(err.type_name(), "RuntimeError");
    assert_eq!(err.to_string(), "char_begin rejected: fs full");

    let daemon = StubDaemon {
        answer: Some(json!({"ack": "char_begin", "ok": false, "error": ""})),
        ..Default::default()
    };
    let err = run(push_character(&daemon, s(&folder))).unwrap_err();
    assert_eq!(err.to_string(), "char_begin rejected: no detail");
}

#[test]
fn push_character_failed_write_raises_runtime_error() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = make_pack(tmp.path(), &[("a.gif", b"x")], None);
    let daemon = StubDaemon {
        fail_writes: true,
        ..Default::default()
    };
    let err = run(push_character(&daemon, s(&folder))).unwrap_err();
    assert_eq!(err.type_name(), "RuntimeError");
    assert_eq!(err.to_string(), "ble write failed for cmd:char_begin");
}

#[test]
fn push_character_expands_home() {
    let home = PathBuf::from("/Users/someone");
    assert_eq!(
        expanduser_with("~/packs/bufo", Some(home.clone())),
        PathBuf::from("/Users/someone/packs/bufo")
    );
    assert_eq!(expanduser_with("~", Some(PathBuf::from("/"))), PathBuf::from("/"));
    assert_eq!(
        expanduser_with("/abs/~x", Some(home)),
        PathBuf::from("/abs/~x")
    );
    assert_eq!(
        expanduser_with("~no-such-user-eve-test/x", None),
        PathBuf::from("~no-such-user-eve-test/x")
    );
}

#[test]
fn resolve_folds_dotdot_after_a_missing_part() {
    let tmp = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(tmp.path()).unwrap();
    let p = tmp.path().join("missing").join("..").join("other");
    assert_eq!(resolve(&p), real.join("other"));
}

#[test]
fn os_error_text_matches_python() {
    let err = PushError::os(
        &io::Error::from_raw_os_error(libc::ENOENT),
        Some(Path::new("/x/a.gif")),
    );
    assert_eq!(err.type_name(), "FileNotFoundError");
    assert_eq!(err.to_string(), "[Errno 2] No such file or directory: '/x/a.gif'");
}

#[test]
fn thousands_matches_python_format() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1000), "1,000");
    assert_eq!(thousands(1_800_000), "1,800,000");
}

// ---- C (test_serial_lean): acks matched to their own request, never lost ------

/// A daemon whose acks go through the real [`AckRouter`], with a board that
/// answers folder-push commands like firmware xfer.h: chunk acks carry n, the
/// file's cumulative byte count. `sync` delivers the ack inside send(), before
/// the sender resumes; otherwise it is delivered a moment later.
struct BoardDaemon {
    router: Arc<AckRouter>,
    sync: bool,
    board: Mutex<BoardState>,
    /// Every ack a waiter received (the Python test's spy on wait_for_ack).
    matched: Mutex<Vec<Value>>,
}

#[derive(Default)]
struct BoardState {
    written: i64,
    /// acks to deliver before the next real one
    queue_first: Vec<Value>,
    /// chunk commands to leave unanswered
    silent_chunks: usize,
    /// set the late ack on the first chunk of the next push
    late_ack_on_first_chunk: bool,
}

impl BoardDaemon {
    fn new(sync: bool) -> Self {
        Self {
            router: Arc::new(AckRouter::new()),
            sync,
            board: Mutex::new(BoardState::default()),
            matched: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl PushTarget for BoardDaemon {
    type Waiter = AckWaiter;

    fn expect_ack(&self, ack_type: &str, n: Option<i64>) -> AckWaiter {
        self.router.expect_ack(ack_type, n)
    }

    async fn send(&self, obj: &Value) -> bool {
        let cmd = obj.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
        let (ack, extra) = {
            let mut b = self.board.lock();
            if cmd == "chunk" && b.late_ack_on_first_chunk {
                b.late_ack_on_first_chunk = false;
                b.queue_first = vec![json!({"ack": "chunk", "ok": true, "n": 300})];
            }
            if cmd == "file" {
                b.written = 0;
            }
            let mut n = 0;
            if cmd == "chunk" {
                let d = obj["d"].as_str().unwrap();
                b.written += base64::engine::general_purpose::STANDARD
                    .decode(d)
                    .unwrap()
                    .len() as i64;
                n = b.written;
                if b.silent_chunks > 0 {
                    b.silent_chunks -= 1;
                    return true;
                }
            }
            (
                json!({"ack": cmd, "ok": true, "n": n}),
                std::mem::take(&mut b.queue_first),
            )
        };
        let router = Arc::clone(&self.router);
        let deliver = async move {
            for e in &extra {
                router.route(e);
            }
            router.route(&ack);
        };
        if self.sync {
            deliver.await;
        } else {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                deliver.await;
            });
        }
        true
    }

    async fn wait_for_ack(
        &self,
        ack_type: &str,
        timeout: Duration,
        waiter: AckWaiter,
    ) -> Result<Value, AckWaitError> {
        let ack = self.router.wait_for_ack(ack_type, timeout, Some(waiter)).await?;
        self.matched.lock().push(ack.clone());
        Ok(ack)
    }
}

fn lean_pack(base: &Path, size: usize) -> PathBuf {
    let folder = base.join("pack");
    std::fs::create_dir_all(&folder).unwrap();
    let mut data: Vec<u8> = (0..=255u8).collect::<Vec<u8>>().repeat(size / 256);
    data.extend(vec![0u8; size % 256]);
    std::fs::write(folder.join("a.gif"), data).unwrap();
    folder
}

fn fast_acks() -> AckTimeouts {
    AckTimeouts {
        fast: Duration::from_secs_f64(0.3),
        slow: Duration::from_secs_f64(0.3),
    }
}

#[test]
fn test_c_ack_before_the_sender_resumes_is_not_lost() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = lean_pack(tmp.path(), 300);
    let d = BoardDaemon::new(true);
    let out = run(push_character_with(&d, s(&folder), fast_acks())).unwrap();
    assert!(out.files == 1 && out.total_bytes == 300);
    assert_eq!(d.router.pending(), 0);
}

#[test]
fn test_c_late_chunk_ack_does_not_satisfy_the_next_chunk() {
    let tmp = tempfile::tempdir().unwrap();
    let first = lean_pack(&tmp.path().join("one"), 300);
    let second = lean_pack(&tmp.path().join("two"), 300);
    let chunk_ns = run(async {
        let d = BoardDaemon::new(false);
        d.board.lock().silent_chunks = 1; // push 1: the first chunk's ack never comes
        let err = push_character_with(&d, s(&first), fast_acks())
            .await
            .unwrap_err();
        assert_eq!(err.type_name(), "TimeoutError");
        // Push 2: a late chunk ack from an earlier request turns up just before
        // push 2's own first chunk ack. Its n (300) cannot answer this chunk
        // (n=200). A late ack whose n happens to equal the expected one is
        // indistinguishable on the wire.
        d.matched.lock().clear();
        d.board.lock().late_ack_on_first_chunk = true;
        push_character_with(&d, s(&second), fast_acks()).await.unwrap();
        let ns: Vec<i64> = d
            .matched
            .lock()
            .iter()
            .filter(|a| a.get("ack").and_then(Value::as_str) == Some("chunk"))
            .map(|a| a["n"].as_i64().unwrap())
            .collect();
        ns
    });
    assert_eq!(chunk_ns, vec![200, 300], "chunk waiters received acks n={chunk_ns:?}");
}

// ---- AckRouter routing rules -------------------------------------------------

#[test]
fn router_failure_ack_matches_a_waiter_that_names_n() {
    run(async {
        let r = AckRouter::new();
        let w = r.expect_ack("chunk", Some(200));
        // firmware failure acks carry n=0, so those still match
        assert!(r.route(&json!({"ack": "chunk", "ok": false, "n": 0, "error": "crc"})));
        let got = r.wait_for_ack("chunk", Duration::from_secs(1), Some(w)).await.unwrap();
        assert_eq!(got["error"], "crc");
    });
}

#[test]
fn router_oldest_waiter_first_and_float_n_matches() {
    run(async {
        let r = AckRouter::new();
        let a = r.expect_ack("file", None);
        let b = r.expect_ack("file", None);
        assert!(r.route(&json!({"ack": "file", "ok": true, "i": 1})));
        assert!(r.route(&json!({"ack": "file", "ok": true, "i": 2})));
        assert!(!r.route(&json!({"ack": "file", "ok": true, "i": 3})));
        let d = Duration::from_secs(1);
        assert_eq!(r.wait_for_ack("file", d, Some(a)).await.unwrap()["i"], 1);
        assert_eq!(r.wait_for_ack("file", d, Some(b)).await.unwrap()["i"], 2);
        let c = r.expect_ack("chunk", Some(200));
        assert!(r.route(&json!({"ack": "chunk", "ok": true, "n": 200.0})));
        assert_eq!(r.wait_for_ack("chunk", d, Some(c)).await.unwrap()["n"], 200.0);
        assert_eq!(r.pending(), 0);
    });
}

#[test]
fn router_dropped_waiter_is_skipped_and_pruned() {
    run(async {
        let r = AckRouter::new();
        let dropped = r.expect_ack("file", None);
        drop(dropped);
        let live = r.expect_ack("file", None);
        assert_eq!(r.pending(), 1); // the cancelled one was pruned on register
        assert!(r.route(&json!({"ack": "file", "ok": true})));
        let got = r
            .wait_for_ack("file", Duration::from_secs(1), Some(live))
            .await
            .unwrap();
        assert_eq!(got["ok"], true);
        let err = r
            .wait_for_ack("file", Duration::from_millis(20), None)
            .await
            .unwrap_err();
        assert_eq!(err, AckWaitError::Timeout);
        assert_eq!(r.pending(), 0);
        assert!(!r.route(&json!({"cmd": "file"})));
    });
}

#[test]
fn constants_match_python() {
    let py = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../bridge/src/eve_bridge/folder_push.py"
    ));
    assert!(py.contains(&format!("CHUNK_SIZE = {CHUNK_SIZE}\n")));
    assert!(py.contains("MAX_TOTAL_BYTES = 1_800_000\n"));
    assert_eq!(MAX_TOTAL_BYTES, 1_800_000);
    assert!(py.contains(&format!("ACK_TIMEOUT_FAST = {ACK_TIMEOUT_FAST:.1}")));
    assert!(py.contains(&format!("ACK_TIMEOUT_SLOW = {ACK_TIMEOUT_SLOW:.1}")));
    assert_eq!(AckTimeouts::default().fast, Duration::from_secs(5));
    assert_eq!(AckTimeouts::default().slow, Duration::from_secs(15));
}

#[test]
fn py_helpers_follow_python() {
    assert_eq!(py_repr_str("bufo"), "'bufo'");
    assert_eq!(py_repr_str("it's"), "\"it's\"");
    assert!(py_truthy(Some(&json!(1))));
    assert!(!py_truthy(Some(&json!(0))));
    assert!(!py_truthy(Some(&json!(""))));
    assert!(!py_truthy(None));
    assert!(py_eq_int(Some(&json!(true)), 1));
    assert!(!py_eq_int(Some(&json!("200")), 200));
    assert_eq!(py_str_value(&json!(5)), "5");
    assert_eq!(py_str_value(&json!(null)), "None");
}
