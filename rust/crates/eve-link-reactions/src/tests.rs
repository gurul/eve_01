//! The store on its own (ids, Telegram's one-reaction semantics, the file): `bridge/tests/test_link_reactions.py`.
//! The inlet and link-door tests in that file drive `TelegramInlet` and `link_chat`; they are ported with those.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// telegram.SEEN_REACTION
const SEEN: &str = "👀";

fn json_file(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn map(pairs: &[(&str, &str)]) -> serde_json::Map<String, Value> {
    pairs.iter().map(|(k, e)| (k.to_string(), Value::from(*e))).collect()
}

#[test]
fn one_reaction_per_message_a_new_one_replaces_it_and_empty_takes_it_off() {
    let r = Reactions::with_clock(None, || 1000.0);
    assert_eq!(r.version(), 1_000_000);
    let heard = Arc::new(Mutex::new(Vec::<String>::new()));
    let h = heard.clone();
    r.add_listener(Arc::new(move |k: &str| {
        h.lock().push(k.to_string());
        Ok(())
    }));
    let mid = r.mint("turn-aaaaaaaa").unwrap();
    assert!(mid < 0 && Reactions::is_app(mid) && !Reactions::is_app(5));
    assert_eq!(r.mint("turn-aaaaaaaa").unwrap(), mid); // stable per message
    assert_ne!(r.mint("turn-bbbbbbbb").unwrap(), mid);
    let v0 = r.version();
    r.set(mid, "👀").unwrap();
    assert_eq!(r.snapshot(), map(&[("turn-aaaaaaaa", "👀")]));
    assert_eq!(r.version(), v0 + 1);
    r.set(mid, "👍").unwrap(); // replaces, never adds
    assert_eq!(r.snapshot(), map(&[("turn-aaaaaaaa", "👍")]));
    assert_eq!(r.get("turn-aaaaaaaa"), "👍");
    r.set(mid, "👍").unwrap(); // the same: no change, no wake
    assert_eq!(r.version(), v0 + 2);
    assert_eq!(*heard.lock(), vec!["turn-aaaaaaaa".to_string(); 2]);
    r.set(mid, "").unwrap();
    assert!(r.snapshot().is_empty());
    assert_eq!(r.version(), v0 + 3);
    assert_eq!(heard.lock().len(), 3);
    r.set(mid, "").unwrap(); // nothing to take off
    assert_eq!(r.version(), v0 + 3);
}

#[test]
fn an_unknown_id_or_a_word_is_refused() {
    let r = Reactions::default();
    assert_eq!(r.set(-99, "👍"), Err(SetError::NoSuchMessage)); // never minted
    let mid = r.mint("turn-aaaaaaaa").unwrap();
    let nine = "👍".repeat(9);
    for bad in ["ok", "<b>", "👍 ", "1", "中", nine.as_str()] {
        assert_eq!(r.set(mid, bad), Err(SetError::NotAnEmoji), "{bad:?}");
    }
    assert_eq!(r.mint("../etc"), Err(BadKey));
    assert!(r.snapshot().is_empty());
    // the messages Python's exceptions carry
    assert_eq!(SetError::NotAnEmoji.to_string(), "not an emoji");
    assert_eq!(SetError::NoSuchMessage.to_string(), "no such app message");
    assert_eq!(BadKey.to_string(), "bad message key");
}

#[test]
fn clean_keeps_only_a_short_emoji() {
    for good in ["👍", "✍", "🏆", "👀", "👍🏽", "❤️", "👨‍👩‍👧"] {
        assert_eq!(clean(good), good);
    }
    let nine = "👍".repeat(9);
    for bad in ["", "a", "ok", " ", "👍 ", "<script>", "中", "é", nine.as_str()] {
        assert_eq!(clean(bad), "", "{bad:?}");
    }
    for bad in [Value::Null, Value::from(7), serde_json::json!(["👍"])] {
        assert_eq!(clean_value(&bad), "");
    }
    assert_eq!(clean_value(&Value::from("🏆")), "🏆");
}

#[test]
fn reactions_are_kept_in_their_file_and_junk_in_it_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let r = Reactions::new(Some(&path));
    r.set(r.mint("turn-aaaaaaaa").unwrap(), "🏆").unwrap();
    assert_eq!(json_file(&path), serde_json::json!({"turn-aaaaaaaa": "🏆"}));
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(Reactions::new(Some(&path)).snapshot(), map(&[("turn-aaaaaaaa", "🏆")])); // after a restart
    fs::write(
        &path,
        serde_json::json!({"turn-aaaaaaaa": "🏆", "../x": "👍", "turn-bbbbbbbb": "<b>hi</b>", "turn-c": 3}).to_string(),
    )
    .unwrap();
    assert_eq!(Reactions::new(Some(&path)).snapshot(), map(&[("turn-aaaaaaaa", "🏆")]));
    fs::write(&path, "not json").unwrap();
    assert!(Reactions::new(Some(&path)).snapshot().is_empty()); // never fails the daemon
}

#[test]
fn eyes_are_never_kept_in_the_file_so_a_crash_cannot_leave_them_on() {
    // Review, 2026-10-09: a 👀 saved before a crash came back on restart with no id left to take it off.
    assert!(is_transient(SEEN));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let r = Reactions::new(Some(&path));
    r.set(r.mint("turn-aaaaaaaa").unwrap(), "🏆").unwrap();
    r.set(r.mint("turn-bbbbbbbb").unwrap(), SEEN).unwrap();
    assert_eq!(r.snapshot(), map(&[("turn-aaaaaaaa", "🏆"), ("turn-bbbbbbbb", "👀")])); // shown while it is true
    assert_eq!(json_file(&path), serde_json::json!({"turn-aaaaaaaa": "🏆"})); // never written
    assert_eq!(Reactions::new(Some(&path)).snapshot(), map(&[("turn-aaaaaaaa", "🏆")])); // the "crash"
    // a file written before this fix, holding a 👀: dropped on load
    fs::write(&path, serde_json::json!({"turn-aaaaaaaa": "🏆", "turn-bbbbbbbb": "👀"}).to_string()).unwrap();
    assert_eq!(Reactions::new(Some(&path)).snapshot(), map(&[("turn-aaaaaaaa", "🏆")]));
}

#[test]
fn only_the_newest_reactions_are_kept() {
    let r = Reactions::default();
    for i in 0..MAX_KEPT + 5 {
        r.set(r.mint(&format!("turn-{i:08}")).unwrap(), "👍").unwrap();
    }
    let kept = r.snapshot();
    assert_eq!(kept.len(), MAX_KEPT);
    assert!(!kept.contains_key("turn-00000000"));
    assert!(kept.contains_key(&format!("turn-{:08}", MAX_KEPT + 4)));
}

// ---- beyond the Python file: the format both daemons share, and the edges -----------------------------------

#[test]
fn the_file_is_byte_for_byte_what_python_writes() {
    // json.dump(..., ensure_ascii=False): ", " and ": " separators, the emoji as itself, oldest set first
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let r = Reactions::new(Some(&path));
    r.set(r.mint("turn-bbbbbbbb").unwrap(), "🏆").unwrap();
    r.set(r.mint("turn-aaaaaaaa").unwrap(), "✍").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"turn-bbbbbbbb": "🏆", "turn-aaaaaaaa": "✍"}"#);
    assert!(!dir.path().join(format!("{FILE_NAME}.tmp")).exists());
    r.set(r.mint("turn-bbbbbbbb").unwrap(), "").unwrap();
    r.set(r.mint("turn-aaaaaaaa").unwrap(), "").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
}

#[test]
fn a_python_written_file_is_read_in_its_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    fs::write(&path, "{\"turn-b\": \"\\ud83c\\udfc6\", \"turn-a\": \"👍\"}").unwrap(); // ensure_ascii=True too
    let r = Reactions::new(Some(&path));
    let keys: Vec<String> = r.snapshot().keys().cloned().collect();
    assert_eq!(keys, ["turn-b", "turn-a"]);
    assert_eq!(r.get("turn-b"), "🏆");
    assert_eq!(r.get("turn-zz"), "");
    for junk in ["[1, 2]", "\"x\"", "null"] {
        fs::write(&path, junk).unwrap();
        assert!(Reactions::new(Some(&path)).snapshot().is_empty());
    }
    fs::write(&path, b"{\"turn-a\": \"\xff\"}").unwrap(); // not UTF-8: starting empty
    assert!(Reactions::new(Some(&path)).snapshot().is_empty());
}

#[test]
fn attach_keeps_a_reaction_set_before_the_file_was_found_and_happens_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    fs::write(&path, r#"{"turn-a": "🏆", "turn-b": "👍"}"#).unwrap();
    let r = Reactions::default();
    r.set(r.mint("turn-c").unwrap(), "✍").unwrap();
    r.set(r.mint("turn-a").unwrap(), SEEN).unwrap();
    r.attach(&path);
    // dict.update: turn-a keeps its place with the newer emoji, turn-c comes after
    assert_eq!(r.snapshot(), map(&[("turn-a", "👀"), ("turn-b", "👍"), ("turn-c", "✍")]));
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"turn-a": "🏆", "turn-b": "👍"}"#); // attach never writes
    let other = dir.path().join("other.json");
    r.attach(&other); // ignored
    assert_eq!(r.path(), Some(path.clone()));
    r.set(r.mint("turn-d").unwrap(), "🏆").unwrap();
    assert!(!other.exists());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{"turn-b": "👍", "turn-c": "✍", "turn-d": "🏆"}"#
    );
}

#[test]
fn attach_keeps_only_the_newest_when_the_file_holds_too_many() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let body: Vec<String> = (0..MAX_KEPT + 3).map(|i| format!("\"k{i}\": \"👍\"")).collect();
    fs::write(&path, format!("{{{}}}", body.join(", "))).unwrap();
    let kept = Reactions::new(Some(&path)).snapshot();
    assert_eq!(kept.len(), MAX_KEPT);
    assert!(!kept.contains_key("k2") && kept.contains_key("k3"));
}

#[test]
fn a_reset_reaction_moves_to_the_newest_place() {
    let r = Reactions::default();
    let a = r.mint("turn-a").unwrap();
    let b = r.mint("turn-b").unwrap();
    r.set(a, "👍").unwrap();
    r.set(b, "👍").unwrap();
    r.set(a, "👍").unwrap(); // no change, yet re-set last as in Python's pop-then-insert
    let keys: Vec<String> = r.snapshot().keys().cloned().collect();
    assert_eq!(keys, ["turn-b", "turn-a"]);
}

#[test]
fn minted_ids_count_down_and_the_oldest_are_let_go() {
    let r = Reactions::default();
    assert_eq!(r.mint("k0").unwrap(), -1);
    assert_eq!(r.mint("k1").unwrap(), -2);
    for i in 2..MAX_MINTED + 1 {
        r.mint(&format!("k{i}")).unwrap();
    }
    // k0 (-1) was let go: setting on it is refused, and minting it again gives a fresh id
    assert_eq!(r.set(-1, "👍"), Err(SetError::NoSuchMessage));
    assert_eq!(r.mint("k1").unwrap(), -2);
    assert_eq!(r.mint("k0").unwrap(), -(MAX_MINTED as i64) - 2);
}

#[test]
fn keys_follow_python_key_re() {
    for good in ["a", "turn-aaaaaaaa", "A_b-9", &"x".repeat(64), "abc\n"] {
        assert!(key_ok(good), "{good:?}");
    }
    for bad in ["", "\n", "a b", "a/b", "../etc", &"x".repeat(65), "abc\n\n", "é", "abc\r"] {
        assert!(!key_ok(bad), "{bad:?}");
    }
}

#[test]
fn a_failing_listener_never_costs_the_reaction() {
    let r = Reactions::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    r.add_listener(Arc::new(|_: &str| anyhow::bail!("boom")));
    r.add_listener(Arc::new(|_: &str| panic!("listener panicked")));
    let counter: Listener = Arc::new(move |_: &str| {
        c.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    r.add_listener(counter.clone());
    assert!(r.has_listener(&counter));
    let mid = r.mint("turn-a").unwrap();
    r.set(mid, "👍").unwrap();
    assert_eq!(r.get("turn-a"), "👍");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(r.remove_listener(&counter) && !r.has_listener(&counter) && !r.remove_listener(&counter));
    r.set(mid, "").unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_file_that_cannot_be_written_keeps_the_reaction_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-dir").join(FILE_NAME);
    let r = Reactions::new(Some(&path));
    r.set(r.mint("turn-a").unwrap(), "🏆").unwrap();
    assert_eq!(r.get("turn-a"), "🏆");
    assert!(!path.exists());
    // a symlink where the temp file goes is never followed
    let path = dir.path().join(FILE_NAME);
    let target = dir.path().join("elsewhere");
    std::os::unix::fs::symlink(&target, dir.path().join(format!("{FILE_NAME}.tmp"))).unwrap();
    let r = Reactions::new(Some(&path));
    r.set(r.mint("turn-a").unwrap(), "🏆").unwrap();
    assert!(!target.exists() && !path.exists());
    assert_eq!(r.get("turn-a"), "🏆");
}

#[test]
fn the_version_starts_at_the_wall_clock_in_milliseconds() {
    let before = (wall_clock() * 1000.0) as i64;
    let v = Reactions::default().version();
    assert!(v >= before && v - before < 60_000);
    assert_eq!(Reactions::with_clock(None, || 1.9999).version(), 1999); // int(): truncated
}
