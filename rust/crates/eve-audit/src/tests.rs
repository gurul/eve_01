//! Port of `bridge/tests/test_audit.py`, plus parity checks for the bytes the
//! Python writes. The memory test lives in `tests/render_memory.rs` (it needs
//! its own counting allocator).

use super::*;
use chrono::FixedOffset;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn obj(v: Value) -> Entry {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn read_lines(p: &Path) -> Vec<Value> {
    fs::read_to_string(p)
        .unwrap()
        .trim()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn collect(p: &Path, f: Filters<'_>) -> Vec<Entry> {
    iter_entries(Some(p), f).collect::<io::Result<Vec<_>>>().unwrap()
}

fn seed(p: &Path, entries: &[Value]) {
    let body: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
    fs::write(p, body.join("\n") + "\n").unwrap();
}

fn render_to_string(opts: &RenderOptions) -> String {
    let mut buf = Vec::new();
    assert_eq!(render(opts, &mut buf, false).unwrap(), 0);
    String::from_utf8(buf).unwrap()
}

fn rec<'a>(session: &'a str, hint: &'a str, matcher: &'a str, decision: Option<&'a str>, source: &'a str) -> Record<'a> {
    Record { session_id: session, tool_name: "Bash", hint, matcher, decision, source, ..Default::default() }
}

#[test]
fn test_default_path_returns_path() {
    let home = dirs::home_dir().unwrap();
    // On all platforms the parent dir is somewhere under ~ (not absolute root).
    let p = default_path_from(None, None, &home);
    assert!(p.starts_with(&home), "{p:?}");
    if std::env::var_os("EVE_BRIDGE_AUDIT").is_none() && std::env::var_os("XDG_DATA_HOME").is_none() {
        assert!(default_path().starts_with(&home));
    }
}

#[test]
fn test_default_path_respects_env_override() {
    let tmp = TempDir::new().unwrap();
    let want = tmp.path().join("audit.jsonl");
    let got = default_path_from(Some(want.to_str().unwrap()), None, Path::new("/home/nobody"));
    assert_eq!(got, want);
}

#[test]
fn default_path_empty_override_is_ignored_and_tilde_expands() {
    let home = Path::new("/home/nobody");
    assert_eq!(default_path_from(Some(""), None, home), default_path_from(None, None, home));
    let expanded = default_path_from(Some("~/logs//a.jsonl"), None, home);
    let real_home = std::env::var("HOME").unwrap_or_default();
    if !real_home.is_empty() {
        assert_eq!(expanded, Path::new(&real_home).join("logs").join("a.jsonl"));
    }
    assert_eq!(default_path_from(Some("/x/./y/"), None, home), PathBuf::from("/x/y"));
}

#[cfg(target_os = "macos")]
#[test]
fn default_path_on_macos_is_library_logs() {
    assert_eq!(
        default_path_from(None, Some("/xdg"), Path::new("/Users/me")),
        PathBuf::from("/Users/me/Library/Logs/eve-bridge-audit.jsonl")
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn default_path_on_linux_follows_xdg() {
    let home = Path::new("/home/me");
    assert_eq!(default_path_from(None, Some("/xdg"), home), PathBuf::from("/xdg/eve-bridge/audit.jsonl"));
    assert_eq!(
        default_path_from(None, Some(""), home),
        PathBuf::from("/home/me/.local/share/eve-bridge/audit.jsonl")
    );
}

#[test]
fn expanduser_matches_posixpath() {
    assert_eq!(expanduser("/abs/~x"), "/abs/~x");
    assert_eq!(expanduser("~no-such-user-eve-audit/x"), "~no-such-user-eve-audit/x");
    if let Ok(h) = std::env::var("HOME") {
        assert_eq!(expanduser("~"), if h.trim_end_matches('/').is_empty() { "/".to_string() } else { h.trim_end_matches('/').to_string() });
    }
}

#[test]
fn test_record_appends_jsonl() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("audit.jsonl");
    let log = AuditLog::new(Some(path.clone()));
    log.record(Record {
        elapsed_s: Some(2.345),
        ..rec("abcdef1234567890", "git push origin main", "ask", Some("allow"), "stick")
    });
    log.record(rec("ffff", "ls", "allow", Some("allow"), "auto_allow"));
    let lines = read_lines(&path);
    assert_eq!(lines.len(), 2);
    let (a, b) = (&lines[0], &lines[1]);
    // Session id is truncated to first 8 chars for readability.
    assert_eq!(a["session"], "abcdef12");
    assert_eq!(a["tool"], "Bash");
    assert_eq!(a["hint"], "git push origin main");
    assert_eq!(a["matcher"], "ask");
    assert_eq!(a["decision"], "allow");
    assert_eq!(a["source"], "stick");
    assert_eq!(a["elapsed_s"], 2.345);
    assert!(a.get("ts").is_some());
    // auto_allow path omits elapsed_s.
    assert!(b.get("elapsed_s").is_none());
}

#[test]
fn test_record_long_hint_is_truncated() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("audit.jsonl");
    let log = AuditLog::new(Some(path.clone()));
    let long_hint = "x".repeat(500);
    log.record(rec("x", &long_hint, "ask", Some("deny"), "stick"));
    let entry = &read_lines(&path)[0];
    assert_eq!(entry["hint"].as_str().unwrap().chars().count(), 200);
}

#[test]
fn record_truncates_by_characters_not_bytes() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("audit.jsonl");
    let log = AuditLog::new(Some(path.clone()));
    let hint = "é".repeat(300);
    log.record(rec("ééééééééé", &hint, "ask", None, "defer"));
    let entry = &read_lines(&path)[0];
    assert_eq!(entry["hint"].as_str().unwrap().chars().count(), 200);
    assert_eq!(entry["session"], "éééééééé");
}

#[test]
fn test_record_creates_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("deep").join("nested").join("audit.jsonl");
    let log = AuditLog::new(Some(path.clone()));
    log.record(rec("x", "ls", "allow", Some("allow"), "auto_allow"));
    assert!(path.exists());
}

/// Daemon must never crash on audit IO failure.
///
/// Force the error portably by pointing the audit path AT an existing directory.
#[test]
fn test_record_swallows_io_errors() {
    let tmp = TempDir::new().unwrap();
    let dir_as_path = tmp.path().join("audit_target");
    fs::create_dir(&dir_as_path).unwrap();
    let log = AuditLog::new(Some(dir_as_path));
    assert!(!log.failure_logged());
    // Should not panic even though writing is impossible.
    log.record(rec("x", "ls", "allow", Some("allow"), "auto_allow"));
    log.record(rec("x", "ls", "allow", Some("allow"), "auto_allow"));
    // Sticky "failure logged" flag suppresses repeated warnings.
    assert!(log.failure_logged());
}

/// defer / ble_disconnected paths return no decision.
#[test]
fn test_record_decision_can_be_none() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("audit.jsonl");
    let log = AuditLog::new(Some(path.clone()));
    log.record(rec("x", "some-tool", "default", None, "defer"));
    let entry = &read_lines(&path)[0];
    assert!(entry["decision"].is_null());
    assert_eq!(entry["source"], "defer");
}

#[test]
fn entry_line_matches_python_json_dumps_bytes() {
    // Python: json.dumps(entry, ensure_ascii=False) + "\n" with the same fields.
    let now = FixedOffset::east_opt(8 * 3600)
        .unwrap()
        .with_ymd_and_hms(2026, 5, 16, 1, 23, 45)
        .unwrap()
        + chrono::Duration::microseconds(123_999);
    let jev = obj(json!({"verdict": "safe", "why": "reads only — ok", "destroys": false, "ms": 412, "p": 0.5}));
    let line = entry_line(
        Record {
            session_id: "abcdef1234567890",
            tool_name: "Bash",
            hint: "echo \"hi\"\n\tdone \u{1}",
            matcher: "ask",
            decision: None,
            source: "jev_shadow",
            elapsed_s: Some(2.0),
            jev: Some(&jev),
        },
        &now,
    );
    assert_eq!(
        line,
        "{\"ts\": \"2026-05-16T01:23:45.123+08:00\", \"session\": \"abcdef12\", \"tool\": \"Bash\", \
         \"hint\": \"echo \\\"hi\\\"\\n\\tdone \\u0001\", \"matcher\": \"ask\", \"decision\": null, \
         \"source\": \"jev_shadow\", \"elapsed_s\": 2.0, \"jev\": {\"verdict\": \"safe\", \
         \"why\": \"reads only — ok\", \"destroys\": false, \"ms\": 412, \"p\": 0.5}}\n"
    );
}

#[test]
fn entry_line_omits_empty_jev_and_rounds_elapsed() {
    let now = FixedOffset::west_opt(5 * 3600 + 30 * 60).unwrap().with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
    let empty = Map::new();
    let line = entry_line(
        Record { elapsed_s: Some(0.0625), jev: Some(&empty), ..rec("s", "h", "m", Some("deny"), "telegram") },
        &now,
    );
    assert!(line.starts_with("{\"ts\": \"2026-01-02T03:04:05.000-05:30\""), "{line}");
    assert!(line.ends_with("\"elapsed_s\": 0.062}\n"), "{line}");
    assert!(!line.contains("jev"));
}

#[test]
fn python_float_repr_and_round() {
    for (x, want) in [
        (2.345, "2.345"),
        (2.0, "2.0"),
        (123456.0, "123456.0"),
        (1e16, "1e+16"),
        (1.5e-5, "1.5e-05"),
        (0.0001, "0.0001"),
        (-0.5, "-0.5"),
        (-0.0, "-0.0"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (1.2345678901234567e20, "1.2345678901234567e+20"),
    ] {
        assert_eq!(py_float_repr(x), want, "{x}");
    }
    assert_eq!(round_ndigits(2.0005, 3), 2.001);
    assert_eq!(round_ndigits(0.1875, 3), 0.188);
    assert_eq!(round_ndigits(1.0625, 3), 1.062);
    assert_eq!(py_float_repr(round_ndigits(-0.0001, 3)), "-0.0");
}

// ---- viewer ----

#[test]
fn test_iter_entries_yields_in_order_and_filters() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    seed(
        &p,
        &[
            json!({"ts": "2026-05-16T00:01:02.000+08:00", "tool": "Bash", "decision": "allow", "source": "auto_allow", "hint": "ls"}),
            json!({"ts": "2026-05-16T00:02:03.000+08:00", "tool": "Bash", "decision": "deny",  "source": "stick",      "hint": "rm -rf /"}),
            json!({"ts": "2026-05-16T00:03:04.000+08:00", "tool": "Edit", "decision": "allow", "source": "stick",      "hint": "file.txt"}),
        ],
    );
    let all_e = collect(&p, Filters::default());
    let tools: Vec<&str> = all_e.iter().map(|e| e["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["Bash", "Bash", "Edit"]);

    let denied = collect(&p, Filters { decision: Some("deny"), ..Default::default() });
    assert!(denied.len() == 1 && denied[0]["hint"] == "rm -rf /");

    let via_stick = collect(&p, Filters { source: Some("stick"), ..Default::default() });
    assert_eq!(via_stick.len(), 2);

    let edits = collect(&p, Filters { tool: Some("Edit"), ..Default::default() });
    assert_eq!(edits.len(), 1);
}

#[test]
fn test_iter_entries_skips_corrupt_lines() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    fs::write(
        &p,
        "{\"ts\":\"2026-05-16T00:00:00.000+08:00\",\"tool\":\"Bash\",\"decision\":\"allow\",\"source\":\"auto_allow\",\"hint\":\"ls\"}\n\
         not json at all\n\
         {\"ts\":\"2026-05-16T00:00:01.000+08:00\",\"tool\":\"Bash\",\"decision\":\"deny\",\"source\":\"stick\",\"hint\":\"rm\"}\n",
    )
    .unwrap();
    assert_eq!(collect(&p, Filters::default()).len(), 2);
}

#[test]
fn test_iter_entries_missing_file_yields_nothing() {
    let tmp = TempDir::new().unwrap();
    assert!(collect(&tmp.path().join("no-such.jsonl"), Filters::default()).is_empty());
}

#[test]
fn test_iter_entries_skips_non_object_json() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    fs::write(&p, "null\n[]\n42\n{\"decision\":\"allow\",\"hint\":\"safe\"}\n").unwrap();
    let got = collect(&p, Filters { decision: Some("allow"), ..Default::default() });
    assert_eq!(got, vec![obj(json!({"decision": "allow", "hint": "safe"}))]);
}

#[test]
fn iter_entries_reads_like_python_text_mode() {
    // Universal newlines, invalid UTF-8 replaced, Python's whitespace strip,
    // and the NaN/Infinity literals json.loads accepts.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"{\"hint\":\"a\"}\r{\"hint\":\"b\"}\r\n");
    bytes.extend_from_slice(b"\x1c {\"hint\":\"c\xff\"} \t\n");
    bytes.extend_from_slice(b"{\"hint\":\"NaN in a string\",\"elapsed_s\":NaN,\"x\":-Infinity}\n");
    bytes.extend_from_slice(b"{\"hint\":\"tail-without-newline\"");
    fs::write(&p, bytes).unwrap();
    let hints: Vec<String> = collect(&p, Filters::default())
        .iter()
        .map(|e| e["hint"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(hints, ["a", "b", "c\u{fffd}", "NaN in a string"]);
}

#[test]
fn test_format_entry_truncates_long_hint() {
    let entry = obj(json!({
        "ts": "2026-05-16T00:00:00.123+08:00",
        "tool": "Bash",
        "decision": "allow",
        "source": "auto_allow",
        "hint": "a".repeat(500),
    }));
    let line = format_entry(&entry, true, 80);
    assert!(line.chars().count() <= 80);
    assert!(line.ends_with('…'));
}

#[test]
fn test_format_entry_shows_time_tool_decision_hint() {
    let entry = obj(json!({
        "ts": "2026-05-16T01:23:45.678+08:00",
        "tool": "Bash",
        "decision": "deny",
        "source": "stick",
        "hint": "git push origin main --force",
    }));
    let line = format_entry(&entry, true, 120);
    assert!(line.contains("01:23:45.678"));
    assert!(line.contains("Bash"));
    assert!(line.contains("deny"));
    assert!(line.contains("stick"));
    assert!(line.contains("git push origin main --force"));
}

#[test]
fn format_entry_columns_match_python() {
    // Python: format_entry({...}, ascii_only=True, width=120)
    let entry = obj(json!({"ts": "2026-05-16T01:23:45.678+08:00", "tool": "MultiEditTool", "source": "jev_risky_deferred", "hint": "a\nb"}));
    assert_eq!(format_entry(&entry, true, 120), "01:23:45.678 MultiEdi —     jev_risky_d a b");
    let bare = obj(json!({"ts": "no-t-here-at-all-really", "decision": ""}));
    assert_eq!(format_entry(&bare, true, 10), "no-t-here-at          —                 ");
    // Width floor: the hint keeps at least 20 columns.
    let long = obj(json!({"hint": "z".repeat(30)}));
    assert!(format_entry(&long, true, 0).ends_with(&format!("{}…", "z".repeat(19))));
}

#[test]
fn test_format_entry_ascii_omits_ansi() {
    let entry = obj(json!({"ts": "2026-05-16T00:00:00.000+08:00", "tool": "Bash", "decision": "allow", "source": "auto_allow", "hint": "ls"}));
    let line = format_entry(&entry, true, 120);
    assert!(!line.contains("\x1b["));
}

#[test]
fn test_format_entry_color_emits_ansi() {
    let entry = obj(json!({"ts": "2026-05-16T00:00:00.000+08:00", "tool": "Bash", "decision": "deny", "source": "stick", "hint": "rm"}));
    let line = format_entry(&entry, false, 120);
    assert!(line.contains("\x1b[31m")); // red for deny
    assert!(line.contains(&format!("{ANSI_YELLOW}stick      {ANSI_RESET}")));
    let allow = obj(json!({"decision": "allow", "source": "timeout"}));
    let line = format_entry(&allow, false, 120);
    assert!(line.contains(&format!("{ANSI_GREEN}allow{ANSI_RESET}")));
    assert!(line.contains(&format!("{ANSI_RED}timeout    {ANSI_RESET}")));
}

#[test]
fn test_render_prints_header_and_last_n_entries() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    let entries: Vec<Value> = (0..10)
        .map(|i| json!({"ts": format!("2026-05-16T00:00:{i:02}.000+08:00"), "tool": "Bash", "decision": "allow", "source": "auto_allow", "hint": format!("cmd-{i}")}))
        .collect();
    seed(&p, &entries);
    let text = render_to_string(&RenderOptions { path: Some(p), last: 3, ascii_only: true, ..Default::default() });
    assert!(text.contains("# audit log:"));
    assert!(text.contains("cmd-9"));
    assert!(text.contains("cmd-8"));
    assert!(text.contains("cmd-7"));
    assert!(!text.contains("cmd-6")); // outside last 3
}

#[test]
fn test_render_missing_file_says_empty() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("no-such.jsonl");
    let text = render_to_string(&RenderOptions { path: Some(p.clone()), last: 20, ascii_only: true, ..Default::default() });
    assert!(text.contains("(empty"));
    assert_eq!(
        text,
        format!("# audit log: {}\n# (empty — no PreToolUse decisions recorded yet)\n", p.display())
    );
}

#[test]
fn render_header_is_dim_on_a_tty_unless_ascii() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("no-such.jsonl");
    let mut buf = Vec::new();
    render(&RenderOptions { path: Some(p.clone()), ..Default::default() }, &mut buf, true).unwrap();
    assert!(String::from_utf8(buf).unwrap().starts_with(&format!("{ANSI_DIM}# audit log: {}{ANSI_RESET}\n", p.display())));
    let mut buf = Vec::new();
    render(&RenderOptions { path: Some(p), ascii_only: true, ..Default::default() }, &mut buf, true).unwrap();
    assert!(!String::from_utf8(buf).unwrap().contains('\x1b'));
}

#[test]
fn test_render_tails_matching_entries_in_order() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    let entries: Vec<Value> = (0..12)
        .map(|i| json!({"tool": "Bash", "decision": if i % 2 == 1 { "deny" } else { "allow" }, "hint": format!("cmd-{i}")}))
        .collect();
    seed(&p, &entries);
    let text = render_to_string(&RenderOptions {
        path: Some(p),
        last: 3,
        decision: Some("deny".into()),
        ascii_only: true,
        ..Default::default()
    });
    let lines: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(lines.len(), 3);
    let tails: Vec<&str> = lines.iter().map(|l| l.rsplit(' ').next().unwrap()).collect();
    assert_eq!(tails, ["cmd-7", "cmd-9", "cmd-11"]);
}

#[test]
fn test_render_non_positive_last_keeps_the_complete_history() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    let entries: Vec<Value> = (0..30).map(|i| json!({"hint": format!("cmd-{i}")})).collect();
    seed(&p, &entries);
    for last in [0, -1] {
        let text = render_to_string(&RenderOptions { path: Some(p.clone()), last, ascii_only: true, ..Default::default() });
        assert_eq!(text.lines().count(), 31);
        assert!(text.contains("cmd-0") && text.contains("cmd-29"));
    }
}

/// A writer the follow loop and the test can both see.
#[derive(Clone, Default)]
struct Shared(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn wait_for(cond: impl Fn() -> bool) -> bool {
    for _ in 0..60 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

#[test]
fn render_follow_streams_new_matching_entries_and_survives_truncation() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("audit.jsonl");
    seed(&p, &[json!({"decision": "deny", "hint": "old"})]);
    let stop = Arc::new(AtomicBool::new(false));
    let out = Shared::default();
    let opts = RenderOptions {
        path: Some(p.clone()),
        decision: Some("deny".into()),
        ascii_only: true,
        follow: true,
        stop: Some(stop.clone()),
        ..Default::default()
    };
    let handle = {
        let mut out = out.clone();
        std::thread::spawn(move || render(&opts, &mut out, false).unwrap())
    };
    assert!(wait_for(|| out.text().contains("old")));
    // Give the loop time to reach EOF before appending.
    std::thread::sleep(Duration::from_millis(200));
    let log = AuditLog::new(Some(p.clone()));
    log.record(rec("s", "skipped-allow", "ask", Some("allow"), "telegram"));
    log.record(rec("s", "new-deny", "ask", Some("deny"), "telegram"));
    assert!(wait_for(|| out.text().contains("new-deny")));
    assert!(!out.text().contains("skipped-allow"));

    // Truncate (rotation): the follower starts over from the top.
    fs::write(&p, "").unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    log.record(rec("s", "after-rotate", "ask", Some("deny"), "telegram"));
    assert!(wait_for(|| out.text().contains("after-rotate")));

    stop.store(true, Ordering::Relaxed);
    assert_eq!(handle.join().unwrap(), 0);
}
