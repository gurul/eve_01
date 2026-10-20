//! Port of `bridge/tests/test_codex_auth.py`: private identity setup with a
//! real fake CLI; no owner credentials or network.

use super::*;
use std::os::unix::fs::symlink;
use std::sync::Arc;

struct Homes {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    private: PathBuf,
}

/// The `homes` fixture: a temp HOME with a personal `.codex` signed in to ChatGPT.
fn homes() -> Homes {
    let tmp = tempfile::tempdir().unwrap();
    // pytest's tmp_path is resolved; macOS temp dirs sit under the /var symlink.
    let root = fs::canonicalize(tmp.path()).unwrap();
    let source = root.join(".codex");
    fs::create_dir(&source).unwrap();
    let personal = serde_json::json!({"auth_mode": "chatgpt", "tokens": {"access_token": "personal-test-token"}});
    fs::write(source.join("auth.json"), personal.to_string()).unwrap();
    fs::write(source.join("config.toml"), "model = \"test-model\"\n").unwrap();
    let private = eve_home_in(&root);
    Homes { _tmp: tmp, root, source, private }
}

fn ready(home: &Path) {
    fs::create_dir_all(home).unwrap();
    fs::write(
        home.join("auth.json"),
        serde_json::json!({"auth_mode": "apikey", "OPENAI_API_KEY": "fixture-api-value"}).to_string(),
    )
    .unwrap();
}

fn write_script(path: &Path, body: &str) -> String {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    path.to_string_lossy().into_owned()
}

/// A fake `codex` that records its call and signs in with whatever stdin holds.
/// Builtins only: the child environment carries no PATH.
fn fake_binary(root: &Path) -> String {
    write_script(
        &root.join("codex-fake"),
        r#"#!/bin/sh
IFS= read -r value
if [ -n "${OPENAI_API_KEY+x}" ]; then has=true; else has=false; fi
printf '{"argc":%s,"argv":["%s","%s"],"stdin":"%s","has_openai_key":%s,"home":"%s"}' "$#" "$1" "$2" "$value" "$has" "$CODEX_HOME" > "$CODEX_HOME/test-login-call.json"
printf '{"auth_mode":"apikey","OPENAI_API_KEY":"%s"}' "$value" > "$CODEX_HOME/auth.json"
"#,
    )
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn ordered(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `caplog`: run `f` with a thread-local subscriber and return the log text.
fn caplog<R>(f: impl FnOnce() -> R) -> (R, String) {
    let buf = Captured::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    (out, text)
}

fn setup_error(result: Result<PathBuf, Error>) -> String {
    match result {
        Err(Error::Setup(SetupError(message))) => message,
        other => panic!("expected SetupError, got {other:?}"),
    }
}

#[test]
fn test_default_plan_environment_is_exactly_unchanged() {
    let h = homes();
    let warned = Warned::default();
    let mut e = env(&[
        ("HOME", "test-home"),
        ("PATH", "test-path"),
        ("USER", "user"),
        ("LOGNAME", "login"),
        ("TMPDIR", "temp"),
        ("LANG", "C"),
        ("CODEX_HOME", "personal-home"),
        ("OPENAI_API_KEY", "test-secret"),
        ("EVE_TELEGRAM_TOKEN", "bot"),
        ("CODEX_APP_TOOLS_PIPE_PATH", "desktop-pipe"),
    ]);
    let expected: IndexMap<String, String> = ENV_KEYS.iter().map(|k| (k.to_string(), e[*k].clone())).collect();
    assert_eq!(launch_environment_with(&e, &h.private, &warned), (expected.clone(), AuthMode::Plan));
    e.insert("EVE_CODEX_AUTH".into(), "plan".into());
    assert_eq!(launch_environment_with(&e, &h.private, &warned), (expected, AuthMode::Plan));
    assert!(warned.lock().is_empty());
}

#[test]
fn test_api_ready_selects_only_private_home() {
    let h = homes();
    ready(&h.private);
    let (child, mode) = launch_environment_with(
        &env(&[
            ("EVE_CODEX_AUTH", "api"),
            ("HOME", "os-home"),
            ("CODEX_HOME", "personal"),
            ("OPENAI_API_KEY", "test-secret"),
        ]),
        &h.private,
        &Warned::default(),
    );
    assert_eq!(mode, "api");
    assert_eq!(child, ordered(&[("HOME", "os-home"), ("CODEX_HOME", &h.private.to_string_lossy())]));
}

#[test]
fn test_missing_or_unsigned_home_falls_back_and_logs_once() {
    let cases: [Option<&str>; 7] = [
        None,
        Some("broken-json"),
        Some("{}"),
        Some("[]"),
        Some(r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"not-api"}"#),
        Some(r#"{"auth_mode":"apikey","OPENAI_API_KEY":""}"#),
        Some(r#"{"auth_mode":"apikey","OPENAI_API_KEY":17}"#),
    ];
    for auth in cases {
        let h = homes();
        if let Some(auth) = auth {
            fs::create_dir_all(&h.private).unwrap();
            fs::write(h.private.join("auth.json"), auth).unwrap();
        }
        let warned = Warned::default();
        let ((), text) = caplog(|| {
            for _ in 0..3 {
                assert_eq!(
                    launch_environment_with(&env(&[("EVE_CODEX_AUTH", "api"), ("HOME", "original")]), &h.private, &warned),
                    (ordered(&[("HOME", "original")]), AuthMode::Plan),
                    "{auth:?}"
                );
            }
        });
        assert_eq!(text.lines().count(), 1, "{auth:?}: {text}");
        assert!(text.contains("using the ChatGPT plan") && text.contains("codex-home"), "{text}");
        assert!(text.contains("WARN"), "{text}");
    }
}

#[test]
fn test_invalid_setting_falls_back() {
    for mode in ["", "API", "automatic"] {
        let h = homes();
        let ((child, got), text) =
            caplog(|| launch_environment_with(&env(&[("EVE_CODEX_AUTH", mode)]), &h.private, &Warned::default()));
        assert_eq!((child, got), (IndexMap::new(), AuthMode::Plan));
        assert!(text.contains("must be plan or api"), "{text}");
    }
}

#[test]
fn test_missing_api_home_keeps_personal_codex_home() {
    // From test_codex_api_metering: a missing Eve home leaves the personal CODEX_HOME.
    let h = homes();
    let personal = h.root.join("personal").to_string_lossy().into_owned();
    let (child, mode) = launch_environment_with(
        &env(&[("EVE_CODEX_AUTH", "api"), ("CODEX_HOME", &personal)]),
        &h.root.join("missing"),
        &Warned::default(),
    );
    assert_eq!(mode, AuthMode::Plan);
    assert_eq!(child["CODEX_HOME"], personal);
}

#[test]
fn test_setup_copies_runtime_not_auth_and_passes_key_only_on_stdin() {
    let h = homes();
    let (source, private) = (&h.source, &h.private);
    let personal = fs::read(source.join("auth.json")).unwrap();
    let original_config = format!(
        "model = \"test-model\"\ncli_auth_credentials_store = \"keyring\"\n\
         forced_login_method = \"chatgpt\"\nforced_chatgpt_workspace_id = \"workspace\"\n\
         [mcp_servers.cua_repl.env]\nCODEX_HOME = {}\n",
        serde_json::to_string(&source.to_string_lossy()).unwrap()
    );
    fs::write(source.join("config.toml"), &original_config).unwrap();
    for relative in [
        "computer-use/config.json",
        "browser/sessions/example.toml",
        "plugins/cache/sample/.mcp.json",
        ".tmp/bundled-marketplaces/catalog.json",
        "chrome-native-hosts-v2.json",
    ] {
        let p = source.join(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let body = if p.extension().is_some_and(|e| e == "json") { "{}" } else { "active = true\n" };
        fs::write(&p, body).unwrap();
    }
    fs::write(source.join("plugins/auth.json"), "must-not-copy").unwrap();
    fs::write(source.join("plugins/.credentials.json"), "must-not-copy").unwrap();
    fs::write(source.join("history.jsonl"), "private-history").unwrap();
    let result = setup_in(&h.root, &fake_binary(&h.root), &env(&[("OPENAI_API_KEY", "fixture-secret")])).unwrap();
    assert!(result == *private && api_ready(private));
    assert_eq!(fs::metadata(private).unwrap().permissions().mode() & 0o7777, 0o700);
    assert_eq!(fs::metadata(private.join("auth.json")).unwrap().permissions().mode() & 0o7777, 0o600);
    assert_eq!(fs::read(source.join("auth.json")).unwrap(), personal);
    assert_eq!(fs::read_to_string(source.join("config.toml")).unwrap(), original_config);
    assert!(!private.join("plugins/auth.json").exists());
    assert!(!private.join("plugins/.credentials.json").exists());
    assert!(!private.join("history.jsonl").exists());
    for relative in COPY_PATHS {
        assert!(private.join(relative).exists(), "{relative}");
    }
    let call: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(private.join("test-login-call.json")).unwrap()).unwrap();
    assert_eq!(call["argc"], 2);
    assert_eq!(call["argv"], serde_json::json!(["login", "--with-api-key"]));
    assert!(call["stdin"] == "fixture-secret" && call["home"] == *private.to_string_lossy());
    assert_eq!(call["has_openai_key"], false);
    let config = fs::read_to_string(private.join("config.toml")).unwrap();
    assert!(config.contains("cli_auth_credentials_store = \"file\"") && config.contains("forced_login_method = \"api\""));
    assert!(!config.contains("forced_chatgpt_workspace_id"));
    assert!(!config.contains(&*source.to_string_lossy()) && config.contains(&*private.to_string_lossy()));
    assert!(!config.contains("fixture-secret"));
    assert_ne!(fs::read(private.join("auth.json")).unwrap(), personal); // positive control: distinct identities
}

#[test]
fn test_repeat_refreshes_config_and_preserves_api_signin_without_key() {
    let h = homes();
    setup_in(&h.root, &fake_binary(&h.root), &env(&[("OPENAI_API_KEY", "fixture-secret")])).unwrap();
    let auth = fs::read(h.private.join("auth.json")).unwrap();
    fs::remove_file(h.private.join("test-login-call.json")).unwrap();
    fs::write(h.source.join("config.toml"), "model = \"new-test-model\"\n").unwrap();
    setup_in(&h.root, "must-not-be-executed", &HashMap::new()).unwrap();
    assert_eq!(fs::read(h.private.join("auth.json")).unwrap(), auth);
    assert!(!h.private.join("test-login-call.json").exists());
    assert!(fs::read_to_string(h.private.join("config.toml")).unwrap().contains("new-test-model"));
}

#[test]
fn test_no_key_means_no_login() {
    let h = homes();
    let message = setup_error(setup_in(&h.root, "must-not-run", &HashMap::new()));
    assert!(message.contains("Set OPENAI_API_KEY"), "{message}");
}

#[test]
fn test_failed_login_does_not_echo_output_or_key() {
    let h = homes();
    let bad = write_script(&h.root.join("codex-bad"), "#!/bin/sh\nIFS= read -r v\nprintf '%s\\n' \"$v\"\nexit 1\n");
    let result = setup_in(&h.root, &bad, &env(&[("OPENAI_API_KEY", "fixture-secret")]));
    let message = setup_error(result);
    assert!(message.contains("login failed"), "{message}");
    assert!(!message.contains("fixture-secret"));
    assert!(!api_ready(&h.private));
}

#[test]
fn test_missing_binary_could_not_run() {
    let h = homes();
    let message = setup_error(setup_in(
        &h.root,
        &h.root.join("nonexistent-codex").to_string_lossy(),
        &env(&[("OPENAI_API_KEY", "fixture-secret")]),
    ));
    assert_eq!(message, "Codex API login could not run; check the installed Codex executable.");
}

#[test]
fn test_login_timeout_kills_child() {
    let h = homes();
    let slow = write_script(&h.root.join("codex-slow"), "#!/bin/sh\nexec /bin/sleep 30\n");
    let started = Instant::now();
    let err = run_login(&slow, &IndexMap::new(), "k", Duration::from_millis(300)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn test_destination_symlink_cannot_reach_personal_home() {
    let h = homes();
    let before = fs::read(h.source.join("auth.json")).unwrap();
    fs::create_dir_all(h.private.parent().unwrap()).unwrap();
    symlink(&h.source, &h.private).unwrap();
    setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert_eq!(fs::read(h.source.join("auth.json")).unwrap(), before);
    assert!(!api_ready(&h.private));
}

#[test]
fn test_symlinked_parent_of_destination_is_refused() {
    let h = homes();
    let elsewhere = h.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(h.root.join(".config")).unwrap();
    symlink(&elsewhere, h.root.join(".config/eve-bridge")).unwrap();
    let message = setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert!(message.contains("symlink parents"), "{message}");
    assert!(!elsewhere.join("codex-home").exists());
}

#[test]
fn test_source_runtime_link_to_credentials_is_refused() {
    let h = homes();
    fs::create_dir(h.source.join("plugins")).unwrap();
    symlink(h.source.join("auth.json"), h.source.join("plugins/innocent.json")).unwrap();
    let message = setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert!(message.contains("credentials"), "{message}");
}

#[test]
fn test_source_runtime_link_outside_home_is_refused() {
    let h = homes();
    let outside = h.root.join("outside.json");
    fs::write(&outside, "{}").unwrap();
    fs::create_dir(h.source.join("plugins")).unwrap();
    symlink(&outside, h.source.join("plugins/escape.json")).unwrap();
    let message = setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert!(message.contains("outside its home"), "{message}");
}

#[test]
fn test_hard_linked_auth_is_never_written() {
    let h = homes();
    fs::create_dir_all(&h.private).unwrap();
    let before = fs::read(h.source.join("auth.json")).unwrap();
    fs::hard_link(h.source.join("auth.json"), h.private.join("auth.json")).unwrap();
    let message = setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert!(message.contains("hard link"), "{message}");
    assert!(!api_ready(&h.private));
    assert_eq!(fs::read(h.source.join("auth.json")).unwrap(), before);
}

#[test]
fn test_source_hard_link_to_auth_is_not_copied() {
    let h = homes();
    fs::create_dir(h.source.join("plugins")).unwrap();
    fs::hard_link(h.source.join("auth.json"), h.source.join("plugins/alias.json")).unwrap();
    let message = setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert!(message.contains("hard link"), "{message}");
}

#[test]
fn test_cyclic_runtime_link_fails_cleanly_before_login() {
    let h = homes();
    fs::create_dir(h.source.join("plugins")).unwrap();
    symlink(h.source.join("plugins"), h.source.join("plugins/loop")).unwrap();
    let message = setup_error(setup_in(&h.root, "must-not-run", &env(&[("OPENAI_API_KEY", "fixture-secret")])));
    assert!(message.contains("cyclic"), "{message}");
    // The staging directory is cleaned up on failure.
    let leftovers: Vec<_> = fs::read_dir(h.private.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".codex-setup-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn test_internal_runtime_links_are_copied_not_shared() {
    let h = homes();
    let version = h.source.join("plugins/cache/example/1.0");
    fs::create_dir_all(&version).unwrap();
    fs::write(version.join(".mcp.json"), serde_json::json!({"CODEX_HOME": h.source.to_string_lossy()}).to_string())
        .unwrap();
    symlink(&version, version.parent().unwrap().join("latest")).unwrap();
    setup_in(&h.root, &fake_binary(&h.root), &env(&[("OPENAI_API_KEY", "fixture-secret")])).unwrap();
    let copied = h.private.join("plugins/cache/example/latest");
    assert!(copied.is_dir() && !is_symlink(&copied));
    let read = |p: &Path| -> serde_json::Value { serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap() };
    assert_eq!(read(&copied.join(".mcp.json"))["CODEX_HOME"], *h.private.to_string_lossy());
    assert_eq!(read(&version.join(".mcp.json"))["CODEX_HOME"], *h.source.to_string_lossy());
}

#[test]
fn test_read_only_runtime_file_copies_with_mode_and_mtime() {
    let h = homes();
    let file = h.source.join("computer-use/readonly.bin");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"bytes").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
    let mtime = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::open(&file).unwrap().set_times(fs::FileTimes::new().set_modified(mtime)).unwrap();
    setup_in(&h.root, &fake_binary(&h.root), &env(&[("OPENAI_API_KEY", "fixture-secret")])).unwrap();
    let copied = fs::metadata(h.private.join("computer-use/readonly.bin")).unwrap();
    assert_eq!(copied.permissions().mode() & 0o777, 0o444);
    assert_eq!(copied.modified().unwrap(), mtime);
}

#[test]
fn test_missing_source_config_still_writes_api_config() {
    let h = homes();
    fs::remove_file(h.source.join("config.toml")).unwrap();
    setup_in(&h.root, &fake_binary(&h.root), &env(&[("OPENAI_API_KEY", "fixture-secret")])).unwrap();
    assert_eq!(
        fs::read_to_string(h.private.join("config.toml")).unwrap(),
        "cli_auth_credentials_store = \"file\"\nforced_login_method = \"api\"\n"
    );
    assert_eq!(fs::metadata(h.private.join("config.toml")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn test_api_config_keeps_table_keys_and_refuses_unsafe_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Same-named keys inside a table are not top-level settings and stay.
    fs::write(&path, "a = 1\r\n  forced_login_method = \"chatgpt\"\n[t]\nforced_login_method = \"x\"\n").unwrap();
    api_config(&path).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "cli_auth_credentials_store = \"file\"\nforced_login_method = \"api\"\na = 1\n[t]\nforced_login_method = \"x\"\n"
    );
    // A multi-line string whose line looks like a key must not be cut.
    let tricky = "note = \"\"\"\nforced_login_method = \"chatgpt\"\n\"\"\"\n";
    fs::write(&path, tricky).unwrap();
    let message = match api_config(&path) {
        Err(Error::Setup(SetupError(m))) => m,
        other => panic!("{other:?}"),
    };
    assert_eq!(message, "Could not safely adapt Codex credential storage configuration.");
    assert_eq!(fs::read_to_string(&path).unwrap(), tricky);
    fs::write(&path, "broken = \n").unwrap();
    assert!(matches!(api_config(&path), Err(Error::Value(_))));
}

#[test]
fn test_resolve_matches_python_non_strict() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    symlink(root.join("missing-target"), root.join("dangling")).unwrap();
    assert_eq!(resolve(&root.join("dangling")), root.join("missing-target"));
    assert_eq!(resolve(&root.join("nope/../x")), root.join("x"));
    symlink(root.join("b"), root.join("a")).unwrap();
    symlink(root.join("a"), root.join("b")).unwrap();
    assert_eq!(resolve(&root.join("a/c")), root.join("a/c"));
    fs::create_dir(root.join("d")).unwrap();
    symlink("../d", root.join("d/up")).unwrap();
    assert_eq!(resolve(&root.join("d/up")), root.join("d"));
}

#[test]
fn test_splitlines_keepends_matches_python() {
    assert_eq!(splitlines_keepends("a\nb\r\nc\rd"), vec!["a\n", "b\r\n", "c\r", "d"]);
    assert_eq!(splitlines_keepends(""), Vec::<&str>::new());
    assert_eq!(splitlines_keepends("x\n"), vec!["x\n"]);
}

#[test]
fn test_api_ready_rejects_symlinked_home_and_hard_links() {
    let h = homes();
    ready(&h.private);
    assert!(api_ready(&h.private));
    let alias = h.root.join("alias-home");
    symlink(&h.private, &alias).unwrap();
    assert!(!api_ready(&alias));
    fs::hard_link(h.private.join("auth.json"), h.root.join("extra.json")).unwrap();
    assert!(!api_ready(&h.private));
}

#[test]
fn test_api_key_whitespace_only_is_not_ready() {
    let h = homes();
    fs::create_dir_all(&h.private).unwrap();
    fs::write(h.private.join("auth.json"), r#"{"auth_mode":"apikey","OPENAI_API_KEY":" \t\u001c "}"#).unwrap();
    assert!(!api_ready(&h.private));
}
