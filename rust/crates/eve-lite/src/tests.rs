//! The small parts, in this process (test_lite_daemon.py's "small parts"): the IPC address, the log, the lines, the
//! crate's documented contract, and what the brain is lent instead of the Mac.

use super::*;

#[test]
fn the_ipc_takes_a_socket_file_only() {
    let default = Path::new("/home/someone/.eve-lite/ipc.sock");
    assert_eq!(ipc_path_from(None, None, default), default);
    assert_eq!(ipc_path_from(None, Some(""), default), default);
    assert_eq!(ipc_path_from(Some("/tmp/elsewhere.sock"), None, default), Path::new("/tmp/elsewhere.sock"));
    // the CLI's --socket wins over the environment's
    assert_eq!(ipc_path_from(Some("/tmp/a.sock"), Some("/tmp/b.sock"), default), Path::new("/tmp/a.sock"));
    assert_eq!(ipc_path_from(None, Some("/tmp/b.sock"), default), Path::new("/tmp/b.sock"));
    assert_eq!(ipc_path_from(Some("  /tmp/padded.sock \n"), None, default), Path::new("/tmp/padded.sock"));
    for tcp in ["127.0.0.1:48765", ":48765", "0.0.0.0:48765", "localhost:48765"] {
        assert_eq!(ipc_path_from(Some(tcp), None, default), default, "{tcp}");
        assert_eq!(ipc_path_from(None, Some(tcp), default), default, "{tcp}");
    }
}

#[test]
fn the_log_is_full_eves_with_urls_and_addresses_kept_out() {
    let names: std::collections::BTreeSet<&str> = QUIET_LOGGERS.into_iter().collect();
    assert_eq!(names, ["aioice", "aiortc", "httpcore", "httpx"].into_iter().collect());
    // a bad level is INFO, not a crash
    assert_eq!(level_of("not-a-level"), tracing::Level::INFO);
    assert_eq!(level_of("debug"), tracing::Level::DEBUG);
    assert_eq!(LOG_FORMAT, "%(asctime)s %(levelname)s %(name)s: %(message)s");
}

#[test]
fn the_lines_are_plain() {
    for line in [PAGE_DOWN, STOPPING, UNKNOWN_EVENT] {
        assert!(line.ends_with('.') && !line.contains('\n') && line.chars().count() < 120, "{line}");
    }
}

#[test]
fn the_module_is_documented_with_its_contract() {
    let src = include_str!("lib.rs");
    let doc: String = src
        .lines()
        .filter_map(|l| l.strip_prefix("//!"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n");
    for evt in ["web_login", "web_login_used", "status", "devices", "revoke", "lan"] {
        assert!(doc.contains(&format!("\"evt\": \"{evt}\"")), "{evt}");
    }
    assert!(doc.contains("exits 75") && doc.contains("NEEDS_OPENROUTER") && doc.contains("READY"));
}

#[test]
fn the_exit_codes_and_labels_are_launchds() {
    assert_eq!((EXIT_OK, EXIT_IN_USE, EXIT_RESTART, EXIT_CONFIG), (0, 2, 75, 78));
    assert_eq!(FULL_PLIST, format!("~/Library/LaunchAgents/{FULL_LABEL}.plist"));
    assert_eq!((READY, NEEDS_OPENROUTER), ("READY", "NEEDS_OPENROUTER"));
}

#[tokio::test]
async fn the_brain_is_lent_none_of_the_mac() {
    assert_eq!(no_screen(), None);
    assert!(nothing::<String>().is_empty());
    assert!(no_work().is_none());
    assert_eq!(no_owner("user=%7B%22id%22%3A1%7D&hash=0"), None);
    assert_eq!(refuse_launch(PathBuf::from("/tmp/project"), "claude".into()).await, eve_brain::telegram::LITE_NOT_HERE);
}

#[test]
fn composio_is_built_only_once_signed_in() {
    let tmp = tempfile::tempdir().unwrap();
    let token = tmp.path().join("composio-connect.json");
    let made = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let seen = made.clone();
    let make: MakeBridge = Arc::new(move |path: &Path| {
        seen.lock().push(path.to_path_buf());
        Arc::new(ConnectApps::new(path)) as Arc<dyn LiteApps>
    });
    assert!(make_bridge_with(&token, &make).is_none());
    // a folder at the token's path is not a sign-in
    std::fs::create_dir(&token).unwrap();
    assert!(make_bridge_with(&token, &make).is_none());
    std::fs::remove_dir(&token).unwrap();
    std::fs::write(&token, "{}").unwrap();
    let bridge = make_bridge_with(&token, &make).expect("built");
    assert!(!bridge.started());
    assert_eq!(*made.lock(), vec![token]);
}

#[test]
fn the_sockets_folder_is_made_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("a").join(".eve-lite");
    private_folder(&folder).unwrap();
    assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
    // one that is there is left as it is
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    private_folder(&folder).unwrap();
    assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(path_bytes(Path::new("/tmp/é")), 7);
}

#[test]
fn a_panic_is_a_named_error() {
    let e = guarded(|| -> i32 { panic!("a part that holds CANARY") }).unwrap_err();
    assert_eq!(error_name(&e), "panic");
    assert_eq!(guarded(|| 3).unwrap(), 3);
    let named = anyhow::Error::new(eve_ipc::NamedError::new("OSError", "no"));
    assert_eq!(error_name(&named), "OSError");
}

#[test]
fn the_stop_waits_are_seconds() {
    assert_eq!(secs(-1.0), Duration::ZERO);
    assert_eq!(secs(f64::NAN), Duration::ZERO);
    assert_eq!(secs(0.05), Duration::from_millis(50));
    assert_eq!(PAGE_RETRY_SECS, [10.0, 30.0, 60.0]);
    assert_eq!((CLOSE_MARKERS_SECS, DRAIN_SECS, PAGE_STOP_SECS), (3.0, 12.0, 5.0));
}

#[test]
fn the_backstop_closes_only_a_conversation_with_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Transcripts::new(TranscriptConfig::new(true, tmp.path().join("t")), None, None);
    let conv = store.new_conv(eve_brain::telegram::CHANNEL);
    assert!(!close_conversation(&store, &conv));
    assert!(store.append(eve_brain::telegram::CHANNEL, &conv, "owner", "say", "a line"));
    assert!(close_conversation(&store, &conv));
    assert!(!close_conversation(&store, &conv)); // closed once
}
