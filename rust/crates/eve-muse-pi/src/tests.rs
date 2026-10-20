//! Port of bridge/tests/test_muse_pi.py (the client part). The Pi itself is never reached: ssh is a fake script.

use super::*;

const HOST: &str = "pi@example.invalid";

#[test]
fn test_target_takes_only_a_plain_user_at_host() {
    assert_eq!(target_from(None), "");
    for bad in ["pi", "-oProxyCommand=x@h", "a@b c", "a@b;rm"] {
        assert_eq!(target_from(Some(bad)), "", "{bad}");
    }
    assert_eq!(target_from(Some(HOST)), HOST);
    assert_eq!(target_from(Some(&format!("  {HOST}\n"))), HOST);
    assert_eq!(target_from(Some("pi@[::1]")), "");
    assert_eq!(target_from(Some("pi@fe80::1")), "pi@fe80::1");
}

#[test]
fn test_the_session_is_made_once_and_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let first = session_id(Some(tmp.path())).unwrap();
    assert!(SESSION.is_match(&first));
    assert_eq!(session_id(Some(tmp.path())).unwrap(), first);
    assert_eq!(std::fs::read_to_string(tmp.path().join(SESSION_FILE)).unwrap(), format!("{first}\n"));
    std::fs::write(tmp.path().join(SESSION_FILE), "not-a-uuid\n").unwrap();
    let fresh = session_id(Some(tmp.path())).unwrap();
    assert_ne!(fresh, "not-a-uuid");
    assert_ne!(fresh, first);
}

#[test]
fn a_session_written_by_python_is_kept_and_the_directory_is_made() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a").join("b");
    let id = session_id(Some(&dir)).unwrap();
    assert!(dir.join(SESSION_FILE).exists());
    assert_eq!(session_id(Some(&dir)).unwrap(), id);
    let python = "0b8f5c4e-1d2a-4c3b-9e8f-7a6b5c4d3e2f";
    std::fs::write(dir.join(SESSION_FILE), format!("  {python}  \n")).unwrap();
    assert_eq!(session_id(Some(&dir)).unwrap(), python);
    // Upper case is not what uuid4() writes, so it is replaced.
    std::fs::write(dir.join(SESSION_FILE), python.to_uppercase()).unwrap();
    assert_ne!(session_id(Some(&dir)).unwrap(), python.to_uppercase());
}

#[test]
fn test_parse_reads_the_reply_and_never_shows_raw_errors() {
    assert_eq!(parse(b"{\"ok\": true, \"reply\": \" PONG \"}\n"), Answer::new(true, "PONG"));
    assert_eq!(parse(b""), Answer::new(false, UNREACHABLE_LINE));
    assert_eq!(parse(b"garbage"), Answer::new(false, UNREACHABLE_LINE));
    assert!(!parse(br#"{"ok": false, "error": "not connected to the Muse"}"#).ok);
    assert!(!parse(br#"{"ok": false, "error": "Traceback boom"}"#).text.contains("Traceback"));
    let late = parse(br#"{"ok": false, "error": "Muse did not finish answering in time", "reply": "half"}"#);
    assert_eq!(late, Answer::new(false, "half"));
}

#[test]
fn parse_says_each_reason_in_its_own_words() {
    assert_eq!(
        parse(br#"{"ok": false, "error": "not connected to the Muse"}"#),
        Answer::new(false, NOT_CONNECTED_LINE)
    );
    assert_eq!(parse(br#"{"ok": false, "error": "no answer in time"}"#), Answer::new(false, LATE_LINE));
    assert_eq!(parse(br#"{"ok": true, "reply": "   "}"#), Answer::new(false, NO_ANSWER_LINE));
    assert_eq!(parse(br#"{"ok": true}"#), Answer::new(false, NO_ANSWER_LINE));
    assert_eq!(parse(br#"{"ok": 1, "reply": 5}"#), Answer::new(false, NO_ANSWER_LINE));
    assert_eq!(parse(br#"{"ok": 0, "reply": "hi"}"#), Answer::new(false, UNREACHABLE_LINE));
    assert_eq!(parse(br#"["ok"]"#), Answer::new(false, UNREACHABLE_LINE));
    assert_eq!(parse(br#"{"ok": false, "error": ["not connected"]}"#), Answer::new(false, NOT_CONNECTED_LINE));
    assert_eq!(parse(br#"{"ok": false, "error": 0}"#), Answer::new(false, UNREACHABLE_LINE));
}

#[test]
fn parse_reads_only_the_last_non_blank_line() {
    let out = b"warning: noise\n{\"ok\": true, \"reply\": \"first\"}\r\n{\"ok\": true, \"reply\": \"last\"}\r\n  \n\n";
    assert_eq!(parse(out), Answer::new(true, "last"));
    assert_eq!(parse(b"{\"ok\": true, \"reply\": \"x\"}\nnot json\n"), Answer::new(false, UNREACHABLE_LINE));
    assert_eq!(parse(b"{\"ok\": true, \"reply\": \"caf\xff\"}"), Answer::new(true, "caf\u{fffd}"));
}

#[test]
fn splitlines_matches_python() {
    assert_eq!(splitlines("a\nb\r\nc\rd\u{2028}e\x0bf\n"), vec!["a", "b", "c", "d", "e", "f"]);
    assert_eq!(splitlines(""), Vec::<&str>::new());
    assert_eq!(splitlines("\n\n"), vec!["", ""]);
}

#[test]
fn the_command_is_key_login_only() {
    assert_eq!(
        command(HOST, "s1"),
        vec![
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=6",
            "-o",
            "ServerAliveInterval=30",
            HOST,
            "/opt/eve-muse/venv-python /opt/eve-muse/ask.py --session-id s1",
        ]
    );
}

#[cfg(unix)]
fn fake_ssh(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let fake = dir.join("ssh");
    std::fs::write(&fake, body).unwrap();
    let mut perms = std::fs::metadata(&fake).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(&fake, perms).unwrap();
    fake
}

#[cfg(unix)]
#[tokio::test]
async fn test_ask_runs_ssh_with_the_message_on_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("args");
    let fake = fake_ssh(
        tmp.path(),
        &format!(
            "#!/bin/sh\nprintf \"%s\\n\" \"$@\" > \"{}\"\nmsg=$(cat)\nprintf '{{\"ok\": true, \"reply\": \"got %s\"}}\\n' \"$msg\"\n",
            log.display()
        ),
    );
    let opts = AskOptions { host: Some(HOST.into()), session: Some("s1".into()), ssh: fake, ..Default::default() };
    let answer = ask("hello", opts).await;
    assert_eq!(answer, Answer::new(true, "got hello"));
    let text = std::fs::read_to_string(&log).unwrap();
    let args: Vec<&str> = text.split('\n').collect();
    assert!(args.contains(&"BatchMode=yes"));
    assert!(args.contains(&HOST));
    assert!(args.contains(&format!("{ASK} --session-id s1").as_str()));
}

#[tokio::test]
async fn test_ask_is_off_without_a_pi() {
    let answer = ask("hi", AskOptions { host: Some(String::new()), ..Default::default() }).await;
    assert_eq!(answer, Answer::new(false, OFF_LINE));
}

#[tokio::test]
async fn ask_without_ssh_is_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = AskOptions {
        host: Some(HOST.into()),
        session: Some("s1".into()),
        ssh: tmp.path().join("no-such-ssh"),
        ..Default::default()
    };
    assert_eq!(ask("hi", opts).await, Answer::new(false, UNREACHABLE_LINE));
}

#[cfg(unix)]
#[tokio::test]
async fn ask_that_runs_too_long_is_cut_off() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = fake_ssh(tmp.path(), "#!/bin/sh\nexec sleep 30\n");
    let opts = AskOptions {
        host: Some(HOST.into()),
        session: Some("s1".into()),
        timeout: 0.3,
        ssh: fake,
    };
    let started = std::time::Instant::now();
    assert_eq!(ask("hi", opts).await, Answer::new(false, LATE_LINE));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[tokio::test]
async fn ask_with_a_failed_ssh_is_unreachable_and_never_shows_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = fake_ssh(tmp.path(), "#!/bin/sh\necho 'Permission denied (publickey).' >&2\nexit 255\n");
    let opts = AskOptions { host: Some(HOST.into()), session: Some("s1".into()), ssh: fake, ..Default::default() };
    assert_eq!(ask("hi", opts).await, Answer::new(false, UNREACHABLE_LINE));
}

#[cfg(unix)]
#[tokio::test]
async fn ask_survives_a_relay_that_never_reads_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = fake_ssh(tmp.path(), "#!/bin/sh\nprintf '{\"ok\": true, \"reply\": \"hi\"}\\n'\n");
    let opts = AskOptions { host: Some(HOST.into()), session: Some("s1".into()), ssh: fake, ..Default::default() };
    let big = "x".repeat(1 << 20);
    assert_eq!(ask(&big, opts).await, Answer::new(true, "hi"));
}

#[test]
fn returncode_reads_signals_as_negative() {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(returncode(&std::process::ExitStatus::from_raw(255 << 8)), Some(255));
        assert_eq!(returncode(&std::process::ExitStatus::from_raw(9)), Some(-9));
    }
    assert_eq!(first_chars("héllo", 2), "hé");
    assert_eq!(first_chars("hi", 200), "hi");
}
