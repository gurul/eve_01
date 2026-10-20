//! Ports of `bridge/tests/test_envfile.py` and `bridge/tests/test_envfile_set_key.py`, plus parity tests for the
//! Python string and path semantics the format depends on.

use super::*;
use std::collections::HashMap;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::Arc;

const CANARY: &str = "CANARY-NOT-A-KEY-7f3c";

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn env() -> HashMap<String, String> {
    HashMap::new()
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn imap(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn chmod(path: &Path, m: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(m)).unwrap();
}

fn leftovers(folder: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    out.sort();
    out
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn home(p: &Path) -> Option<OsString> {
    Some(p.as_os_str().to_owned())
}

// --- test_envfile.py ------------------------------------------------------------------------------------------

#[test]
fn test_parse_basic_lines() {
    let text = "# a comment\n\
                \n\
                OPENAI_API_KEY=sk-test-123\n\
                export EVE_NOTES_MODEL=gpt-5-mini\n\
                QUOTED=\"with spaces\"\n\
                SINGLE='x=y'\n  SPACED =  value  \n";
    assert_eq!(
        parse_env_file(text),
        imap(&[
            ("OPENAI_API_KEY", "sk-test-123"),
            ("EVE_NOTES_MODEL", "gpt-5-mini"),
            ("QUOTED", "with spaces"),
            ("SINGLE", "x=y"),
            ("SPACED", "value"),
        ])
    );
}

#[test]
fn test_parse_skips_malformed_lines() {
    assert_eq!(parse_env_file("no equals\n=novalue\n1BAD=x\nGOOD=1\n"), imap(&[("GOOD", "1")]));
}

#[test]
fn test_load_fills_gaps_but_never_overrides() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "A=from-file\nB=from-file\n").unwrap();
    let mut e = map(&[("A", "from-shell")]);
    assert_eq!(load_env_file(Some(&f), &mut e).unwrap(), vec!["B"]);
    assert_eq!(e, map(&[("A", "from-shell"), ("B", "from-file")]));
}

#[test]
fn test_load_missing_file_is_noop() {
    let d = tmp();
    let mut e = env();
    assert!(load_env_file(Some(&d.path().join("absent")), &mut e).unwrap().is_empty());
    assert!(e.is_empty());
}

#[test]
fn test_load_expands_user() {
    let d = tmp();
    fs::write(d.path().join("env"), "K=v\n").unwrap();
    let mut e = env();
    assert_eq!(load_env_file_in(Some(Path::new("~/env")), &mut e, home(d.path())).unwrap(), vec!["K"]);
    assert_eq!(e["K"], "v");
}

#[test]
fn test_load_explicit_shared_env_override() {
    let d = tmp();
    let shared = d.path().join("windows-env");
    // encoding="utf-8-sig": the file starts with a BOM.
    fs::write(&shared, "\u{feff}EVE_LEARNING_PROVIDER=openrouter\nOPENROUTER_API_KEY=test-only\n").unwrap();
    let mut e = map(&[("EVE_ENV_FILE", shared.to_str().unwrap())]);
    load_env_file(None, &mut e).unwrap();
    assert_eq!(e["EVE_LEARNING_PROVIDER"], "openrouter");
    assert_eq!(e["OPENROUTER_API_KEY"], "test-only");
}

#[test]
fn test_explicit_path_wins_over_env_override() {
    let d = tmp();
    let shared = d.path().join("shared");
    let explicit = d.path().join("explicit");
    fs::write(&shared, "A=shared\n").unwrap();
    fs::write(&explicit, "A=explicit\n").unwrap();
    let mut e = map(&[("EVE_ENV_FILE", shared.to_str().unwrap())]);
    load_env_file(Some(&explicit), &mut e).unwrap();
    assert_eq!(e["A"], "explicit");
}

// --- test_envfile_set_key.py ----------------------------------------------------------------------------------

#[test]
fn test_a_new_file_is_private_and_so_is_a_new_folder() {
    let d = tmp();
    let f = d.path().join("cfg").join("eve-bridge").join("env");
    set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap();
    assert_eq!(read(&f), format!("OPENROUTER_API_KEY={CANARY}\n"));
    assert_eq!(mode(&f), 0o600);
    assert_eq!(mode(f.parent().unwrap()), 0o700);
    assert!(leftovers(f.parent().unwrap()).is_empty());
}

#[test]
fn test_a_key_is_replaced_where_it_is_and_every_other_line_is_kept() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(
        &f,
        "# eve lite\nEVE_PROFILE=lite\n\nexport OPENROUTER_API_KEY=old-value\na line the parser skips\nOTHER='x y'\n",
    )
    .unwrap();
    chmod(&f, 0o644);
    set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap();
    assert_eq!(
        read(&f),
        format!("# eve lite\nEVE_PROFILE=lite\n\nOPENROUTER_API_KEY={CANARY}\na line the parser skips\nOTHER='x y'\n")
    );
    assert_eq!(mode(&f), 0o600); // tightened, whatever it was
    assert!(leftovers(d.path()).is_empty());
}

#[test]
fn test_a_name_set_twice_ends_as_one_line_where_it_first_was() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "A=1\nK=first\nB=2\nK=second\n").unwrap();
    assert_eq!(parse_env_file(&read(&f))["K"], "second"); // the loader reads the last one
    set_key(&f, "K", "new").unwrap();
    assert_eq!(read(&f), "A=1\nK=new\nB=2\n");
}

#[test]
fn test_a_new_name_goes_at_the_end_even_without_a_final_newline() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "EVE_PROFILE=lite").unwrap();
    set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap();
    assert_eq!(read(&f), format!("EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n"));
}

#[test]
fn test_a_name_inside_another_name_is_left_alone() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "MY_OPENROUTER_API_KEY=keep\nOPENROUTER_API_KEY_OLD=keep\n# OPENROUTER_API_KEY=commented\n").unwrap();
    set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap();
    assert_eq!(
        read(&f),
        format!(
            "MY_OPENROUTER_API_KEY=keep\nOPENROUTER_API_KEY_OLD=keep\n# OPENROUTER_API_KEY=commented\nOPENROUTER_API_KEY={CANARY}\n"
        )
    );
}

#[test]
fn test_every_value_reads_back_unchanged() {
    let values = [
        CANARY,
        "with spaces inside",
        " leading",
        "trailing ",
        "\ttabbed\t",
        "'single'",
        "\"double\"",
        "it's",
        "say \"hi\"",
        "a=b#c",
        "",
        "'",
        "\"\"",
        "''",
        "x ",
        "\u{1f}",
        "$HOME",
        "ünïcode ✓",
    ];
    for value in values {
        let d = tmp();
        let f = d.path().join("env");
        fs::write(&f, "BEFORE=1\n").unwrap();
        set_key(&f, "V", value).unwrap();
        assert_eq!(parse_env_file(&read(&f))["V"], value, "value {value:?}");
        let mut e = env();
        load_env_file(Some(&f), &mut e).unwrap();
        assert_eq!(e, map(&[("BEFORE", "1"), ("V", value)]), "value {value:?}");
    }
}

#[test]
fn test_a_plain_value_is_written_bare() {
    let d = tmp();
    let f = d.path().join("env");
    set_key(&f, "V", "plain value").unwrap();
    set_key(&f, "Q", " needs quotes").unwrap();
    assert_eq!(read(&f), "V=plain value\nQ=\" needs quotes\"\n");
}

#[test]
fn test_a_value_that_would_start_another_line_is_refused() {
    let mut bads: Vec<char> = LINE_BREAKS.to_vec();
    bads.sort();
    bads.push('\0');
    for bad in bads {
        let d = tmp();
        let f = d.path().join("env");
        fs::write(&f, "KEEP=1\n").unwrap();
        let before = fs::read(&f).unwrap();
        if bad != '\0' {
            // control: the loader would read a second line
            assert_eq!(py_splitlines(&format!("A=x{bad}EVIL=y")).len(), 2);
        }
        let e = set_key(&f, "OPENROUTER_API_KEY", &format!("x{bad}EVIL=y")).unwrap_err();
        assert!(e.is_value_error());
        assert!(!e.to_string().contains("EVIL"));
        assert_eq!(fs::read(&f).unwrap(), before);
        assert!(leftovers(d.path()).is_empty());
    }
}

#[test]
fn test_the_refused_set_is_every_line_break_the_parser_knows() {
    // What Python 3.13 prints for {chr(i) for i in range(0x110000) if len(f"a{chr(i)}b".splitlines()) > 1}.
    let python: Vec<u32> = vec![0xa, 0xb, 0xc, 0xd, 0x1c, 0x1d, 0x1e, 0x85, 0x2028, 0x2029];
    let mut ours: Vec<u32> = LINE_BREAKS.iter().map(|c| *c as u32).collect();
    ours.sort();
    assert_eq!(ours, python);
    // And the splitter breaks on exactly that set, across all of Unicode.
    let breaks: Vec<u32> = (0..0x110000u32)
        .filter_map(char::from_u32)
        .filter(|c| py_splitlines(&format!("a{c}b")).len() > 1)
        .map(|c| c as u32)
        .collect();
    assert_eq!(breaks, python);
}

#[test]
fn test_a_name_the_parser_would_not_read_is_refused() {
    // Python's last case, None, has no Rust counterpart: a name is always a &str.
    for name in ["", "1BAD", "A-B", "A B", "NAME\n", "export X", "Ä"] {
        let d = tmp();
        let f = d.path().join("env");
        assert!(matches!(set_key(&f, name, "v"), Err(EnvFileError::InvalidName)), "{name:?}");
        assert!(matches!(remove_key(&f, name), Err(EnvFileError::InvalidName)), "{name:?}");
        assert!(!f.exists());
    }
}

#[test]
fn test_a_symlinked_file_is_written_through_to_its_target() {
    let d = tmp();
    let real_file = d.path().join("dotfiles").join("eve-env");
    fs::create_dir(real_file.parent().unwrap()).unwrap();
    fs::write(&real_file, "A=1\n").unwrap();
    let link = d.path().join("env");
    symlink(&real_file, &link).unwrap();
    set_key(&link, "OPENROUTER_API_KEY", CANARY).unwrap();
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(read(&real_file), format!("A=1\nOPENROUTER_API_KEY={CANARY}\n"));
    assert_eq!(mode(&real_file), 0o600);
}

#[test]
fn test_a_failed_write_keeps_the_old_file_and_leaves_no_temporary() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "EVE_PROFILE=lite\n").unwrap();
    let fail = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("disk full")) };
    let e = set_key_with(&f, "OPENROUTER_API_KEY", CANARY, &fail, process_home()).unwrap_err();
    assert!(matches!(e, EnvFileError::Io(_)));
    assert_eq!(read(&f), "EVE_PROFILE=lite\n");
    assert!(leftovers(d.path()).is_empty());
}

#[test]
fn test_a_file_that_cannot_be_read_is_never_overwritten() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, b"A=1\n\xff\xfe broken\n").unwrap();
    let e = set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap_err(); // UnicodeDecodeError
    assert!(e.is_value_error());
    assert_eq!(e.to_string(), "'utf-8' codec can't decode byte 0xff in position 4: invalid start byte");
    assert!(remove_key(&f, "A").unwrap_err().is_value_error());
    assert_eq!(fs::read(&f).unwrap(), b"A=1\n\xff\xfe broken\n");
}

#[test]
fn test_remove_key_deletes_every_line_for_the_name() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "# keep me\nEVE_PROFILE=lite\nOPENROUTER_API_KEY=a\nexport EVE_PROFILE=full\nB=2\n").unwrap();
    chmod(&f, 0o644);
    assert!(remove_key(&f, "EVE_PROFILE").unwrap());
    assert_eq!(read(&f), "# keep me\nOPENROUTER_API_KEY=a\nB=2\n");
    assert_eq!(mode(&f), 0o600);
    assert!(leftovers(d.path()).is_empty());
}

#[test]
fn test_remove_key_writes_nothing_when_the_name_is_not_there() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "A=1\n").unwrap();
    chmod(&f, 0o644);
    let stamp = fs::metadata(&f).unwrap().modified().unwrap();
    assert!(!remove_key(&f, "OPENROUTER_API_KEY").unwrap());
    assert_eq!(read(&f), "A=1\n");
    assert_eq!(fs::metadata(&f).unwrap().modified().unwrap(), stamp);
    assert_eq!(mode(&f), 0o644); // nothing was written, so nothing was changed
}

#[test]
fn test_remove_key_on_a_missing_file_creates_nothing() {
    let d = tmp();
    let f = d.path().join("no-such").join("env");
    assert!(!remove_key(&f, "EVE_PROFILE").unwrap());
    assert!(!f.parent().unwrap().exists());
}

#[test]
fn test_the_daemon_loads_what_set_key_wrote() {
    let d = tmp();
    let f = d.path().join("env");
    set_key(&f, "EVE_PROFILE", "lite").unwrap();
    set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap();
    let mut e = env();
    assert_eq!(load_env_file(Some(&f), &mut e).unwrap(), vec!["EVE_PROFILE", "OPENROUTER_API_KEY"]);
    assert_eq!(e, map(&[("EVE_PROFILE", "lite"), ("OPENROUTER_API_KEY", CANARY)]));
}

#[test]
fn test_env_path_is_the_file_load_env_file_reads() {
    let d = tmp();
    let shared = d.path().join("shared-env");
    let mut environ = map(&[("EVE_ENV_FILE", shared.to_str().unwrap())]);
    assert_eq!(env_path(&environ).unwrap(), shared);
    set_key(&env_path(&environ).unwrap(), "K", "from-the-writer").unwrap();
    load_env_file(None, &mut environ).unwrap();
    assert_eq!(environ["K"], "from-the-writer");
    let h = home(&d.path().join("home"));
    let default = configured_path(&env(), h.clone()).unwrap();
    assert_eq!(default, d.path().join("home").join(".config").join("eve-bridge").join("env"));
    set_key_with(&default, "K", "default", &os_replace, h.clone()).unwrap();
    let mut plain = env();
    load_env_file_in(None, &mut plain, h).unwrap();
    assert_eq!(plain, map(&[("K", "default")]));
}

#[test]
fn test_concurrent_writers_lose_no_line() {
    let d = tmp();
    let f = Arc::new(d.path().join("env"));
    fs::write(&*f, "EVE_PROFILE=lite\n").unwrap();
    let threads: Vec<_> = (0..6)
        .map(|n| {
            let f = Arc::clone(&f);
            std::thread::spawn(move || {
                for i in 0..20 {
                    set_key(&f, &format!("K_{n}_{i}"), &i.to_string()).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let parsed = parse_env_file(&read(&f));
    assert_eq!(parsed.len(), 1 + 6 * 20);
    assert_eq!(parsed["EVE_PROFILE"], "lite");
    assert!(leftovers(d.path()).is_empty());
}

thread_local! {
    static LOG: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Log lines go to a buffer of the thread that emitted them. The subscriber is the global default (a scoped
/// `with_default` one races with the other tests' threads over tracing's per-callsite interest cache).
struct ThreadLog;

impl io::Write for ThreadLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        LOG.with(|l| l.borrow_mut().extend_from_slice(buf));
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn capture_logs() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(|| ThreadLog)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other global subscriber in these tests");
    });
    LOG.with(|l| l.borrow_mut().clear());
}

fn log_messages() -> Vec<String> {
    LOG.with(|l| String::from_utf8_lossy(&l.borrow()).lines().map(str::to_string).collect())
}

#[test]
fn test_only_the_name_reaches_the_log() {
    capture_logs();
    let d = tmp();
    let f = d.path().join("env");
    tracing::debug!("control {}", CANARY);
    assert!(log_messages().iter().any(|m| m.contains(CANARY))); // the capture sees a planted canary
    LOG.with(|l| l.borrow_mut().clear());
    set_key(&f, "OPENROUTER_API_KEY", CANARY).unwrap();
    set_key(&f, "OPENROUTER_API_KEY", &format!("{CANARY}-2")).unwrap();
    assert!(remove_key(&f, "OPENROUTER_API_KEY").unwrap());
    assert!(set_key(&f, "OPENROUTER_API_KEY", &format!("{CANARY}\n")).unwrap_err().is_value_error());
    let messages = log_messages();
    assert!(!messages.is_empty() && messages.iter().all(|m| m.contains("OPENROUTER_API_KEY")));
    assert!(!messages.iter().any(|m| m.contains(CANARY)));
}

// --- tests_ref: test_suite_isolation.py (the envfile part) ----------------------------------------------------

#[test]
fn test_an_unset_env_file_reads_the_default_under_home_and_another_file_still_loads() {
    let d = tmp();
    let mut environ = env();
    // A home with no env file in it loads nothing.
    assert!(load_env_file_in(None, &mut environ, home(&d.path().join("home"))).unwrap().is_empty());
    assert!(environ.is_empty());
    let own = d.path().join("env");
    fs::write(&own, "EVE_TEST_ISOLATION=1\n").unwrap();
    assert_eq!(load_env_file(Some(&own), &mut environ).unwrap(), vec!["EVE_TEST_ISOLATION"]);
}

// --- parity with Python's string and path semantics -----------------------------------------------------------

#[test]
fn py_space_is_pythons_isspace() {
    // What Python 3.13 prints for [hex(i) for i in range(0x110000) if chr(i).isspace()].
    let python: Vec<u32> = vec![
        0x9, 0xa, 0xb, 0xc, 0xd, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x85, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003,
        0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
    ];
    let ours: Vec<u32> = (0..0x110000u32).filter_map(char::from_u32).filter(|c| is_py_space(*c)).map(|c| c as u32).collect();
    assert_eq!(ours, python);
}

#[test]
fn splitlines_matches_python() {
    assert_eq!(py_splitlines(""), Vec::<&str>::new());
    assert_eq!(py_splitlines("a\n"), vec!["a"]);
    assert_eq!(py_splitlines("a\n\nb"), vec!["a", "", "b"]);
    assert_eq!(py_splitlines("a\r\nb\rc\u{2028}d"), vec!["a", "b", "c", "d"]);
    assert_eq!(py_splitlines("a\r\r\nb"), vec!["a", "", "b"]);
    assert_eq!(py_splitlines("\n"), vec![""]);
}

#[test]
fn parse_edge_cases_match_python() {
    // Python: parse_env_file of each line below.
    assert_eq!(parse_env_file("export   A=1"), imap(&[("A", "1")]));
    assert_eq!(parse_env_file("exportA=1"), imap(&[("exportA", "1")]));
    assert_eq!(parse_env_file("export=1"), imap(&[("export", "1")]));
    assert_eq!(parse_env_file("A='mismatch\""), imap(&[("A", "'mismatch\"")]));
    assert_eq!(parse_env_file("A=\"'\""), imap(&[("A", "'")]));
    assert_eq!(parse_env_file("A= x # not a comment"), imap(&[("A", "x # not a comment")]));
    assert_eq!(parse_env_file("\u{1f}A=\u{1f}v\u{1f}"), imap(&[("A", "v")]));
    assert_eq!(parse_env_file("A=1\nB=2\nA=3"), imap(&[("A", "3"), ("B", "2")]));
    assert_eq!(parse_env_file("\u{feff}A=1"), imap(&[])); // a BOM is not whitespace to the parser
    assert_eq!(parse_env_file("  # c\n"), imap(&[]));
}

#[test]
fn quoting_is_minimal() {
    assert_eq!(quoted("plain"), "plain");
    assert_eq!(quoted(""), "");
    assert_eq!(quoted("'"), "'");
    assert_eq!(quoted("''"), "\"''\"");
    assert_eq!(quoted("\u{1f}"), "\"\u{1f}\"");
    assert_eq!(quoted("a\u{a0}"), "\"a\u{a0}\"");
}

#[test]
fn a_bom_and_crlf_file_is_rewritten_with_plain_newlines() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "\u{feff}A=1\r\nB=2\r\n").unwrap();
    set_key(&f, "C", "3").unwrap();
    assert_eq!(read(&f), "A=1\nB=2\nC=3\n");
}

#[test]
fn expanduser_matches_pathlib() {
    let h = || Some(OsString::from("/Users/someone/"));
    assert_eq!(expanduser_in(Path::new("~"), h()).unwrap(), PathBuf::from("/Users/someone"));
    assert_eq!(expanduser_in(Path::new("~/a//b/./c/"), h()).unwrap(), PathBuf::from("/Users/someone/a/b/c"));
    assert_eq!(expanduser_in(Path::new("./~/x"), h()).unwrap(), PathBuf::from("/Users/someone/x"));
    assert_eq!(expanduser_in(Path::new("a/~"), h()).unwrap(), PathBuf::from("a/~"));
    assert_eq!(expanduser_in(Path::new("/~/x"), h()).unwrap(), PathBuf::from("/~/x"));
    // An empty HOME is the root, as os.path.expanduser makes it.
    assert_eq!(expanduser_in(Path::new("~/x"), Some(OsString::new())).unwrap(), PathBuf::from("/x"));
    // A home that still starts with "~" is pathlib's RuntimeError.
    assert!(matches!(expanduser_in(Path::new("~/x"), Some(OsString::from("~nope"))), Err(EnvFileError::HomeDir)));
    assert!(matches!(
        expanduser_in(Path::new("~no-such-user-eve-envfile/x"), h()),
        Err(EnvFileError::HomeDir)
    ));
    // HOME unset falls back to the password database.
    let pw = expanduser_in(Path::new("~"), None).unwrap();
    assert!(pw.is_absolute());
    assert_eq!(expanduser_in(Path::new(""), h()).unwrap(), PathBuf::from("."));
}

#[test]
fn realpath_matches_python() {
    let d = tmp();
    let base = fs::canonicalize(d.path()).unwrap();
    fs::create_dir(base.join("dir")).unwrap();
    symlink(base.join("dir"), base.join("abs")).unwrap();
    symlink("dir", base.join("rel")).unwrap();
    symlink("loop2", base.join("loop1")).unwrap();
    symlink("loop1", base.join("loop2")).unwrap();
    symlink("missing/deeper", base.join("dangling")).unwrap();
    assert_eq!(realpath(&base.join("abs/env")).unwrap(), base.join("dir/env"));
    assert_eq!(realpath(&base.join("rel/x/../env")).unwrap(), base.join("dir/env"));
    assert_eq!(realpath(&base.join("nope/a/b")).unwrap(), base.join("nope/a/b"));
    // A loop is kept as written, not an error.
    assert_eq!(realpath(&base.join("loop1/env")).unwrap(), base.join("loop1/env"));
    assert_eq!(realpath(&base.join("dangling")).unwrap(), base.join("missing/deeper"));
    assert_eq!(realpath(Path::new("/..")).unwrap(), PathBuf::from("/"));
    // Relative paths resolve from the working directory.
    assert_eq!(realpath(Path::new("x")).unwrap(), fs::canonicalize(".").unwrap().join("x"));
}

#[test]
fn an_unreadable_env_file_loads_nothing() {
    let d = tmp();
    // A folder where the file should be: reading fails with an OSError other than "not found".
    let mut e = env();
    assert!(load_env_file(Some(d.path()), &mut e).unwrap().is_empty());
    assert!(e.is_empty());
}

#[test]
fn a_non_utf8_env_file_is_an_error_on_load() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, b"A=\xe2\x82").unwrap();
    let err = load_env_file(Some(&f), &mut env()).unwrap_err();
    assert_eq!(err.to_string(), "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data");
    fs::write(&f, b"A=\xe2(x").unwrap();
    let err = load_env_file(Some(&f), &mut env()).unwrap_err();
    assert_eq!(err.to_string(), "'utf-8' codec can't decode byte 0xe2 in position 2: invalid continuation byte");
}

#[test]
fn a_nul_value_is_refused_by_the_process_environment_only() {
    let d = tmp();
    let f = d.path().join("env");
    fs::write(&f, "EVE_ENVFILE_NUL_TEST=a\0b\n").unwrap();
    let mut e = env();
    assert_eq!(load_env_file(Some(&f), &mut e).unwrap(), vec!["EVE_ENVFILE_NUL_TEST"]);
    assert_eq!(e["EVE_ENVFILE_NUL_TEST"], "a\0b");
    let err = load_env_file(Some(&f), &mut ProcessEnv).unwrap_err();
    assert!(matches!(err, EnvFileError::EmbeddedNull) && err.is_value_error());
    assert!(std::env::var_os("EVE_ENVFILE_NUL_TEST").is_none());
}

#[test]
fn oserror_text_matches_python() {
    let e = io::Error::from_raw_os_error(libc::EACCES);
    assert_eq!(py_oserror(&e, Path::new("/x/env")), "[Errno 13] Permission denied: '/x/env'");
}

#[test]
fn the_empty_env_file_var_means_the_default() {
    let d = tmp();
    let h = home(d.path());
    let environ = map(&[("EVE_ENV_FILE", "")]);
    assert_eq!(configured_path(&environ, h).unwrap(), d.path().join(".config/eve-bridge/env"));
}
