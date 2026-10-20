//! Config-home resolution. Port of `bridge/tests/test_claude_home.py`, plus cover for the pathlib details the
//! Rust side reimplements (expansion, normalization, the install-time union).
//!
//! Regression cover for the failure this module exists to prevent: hooks installed into ~/.claude while
//! sessions run against $CLAUDE_CONFIG_DIR, so nothing ever prompts and nothing ever warns.
//!
//! pytest's autouse `_clean_env` fixture becomes [`clean_env`]: a [`HomeEnv`] with neither variable set and
//! `$HOME` at a temp dir, so no test reads or writes the process environment.

use super::*;

fn os(s: impl AsRef<OsStr>) -> OsString {
    s.as_ref().to_os_string()
}

/// A clean environment: `$CLAUDE_CONFIG_DIR` and `$EVE_CLAUDE_CONFIG_DIRS` unset, `$HOME` a temp dir.
fn clean_env() -> (tempfile::TempDir, HomeEnv) {
    let home = tempfile::tempdir().unwrap();
    let env = HomeEnv { claude_config_dir: None, claude_config_dirs: None, home: Some(os(home.path())) };
    (home, env)
}

fn home(env: &HomeEnv) -> PathBuf {
    env.home_dir().unwrap()
}

fn pathsep() -> String {
    (PATHSEP as char).to_string()
}

#[test]
fn test_defaults_to_dot_claude() {
    let (_h, env) = clean_env();
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), home(&env).join(".claude"));
    assert_eq!(settings_path_in(&env, None).unwrap(), home(&env).join(".claude").join("settings.json"));
}

#[test]
fn test_env_config_dir_wins_over_default() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    env.claude_config_dir = Some(os(tmp.path()));
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), tmp.path());
    assert_eq!(settings_path_in(&env, None).unwrap(), tmp.path().join("settings.json"));
}

#[test]
fn test_explicit_override_wins_over_env() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    env.claude_config_dir = Some(os(tmp.path().join("from-env")));
    let explicit = tmp.path().join("explicit");
    assert_eq!(claude_config_dir_in(&env, Some(explicit.as_os_str())).unwrap(), explicit);
}

#[test]
fn test_blank_env_falls_back_to_default() {
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("   "));
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), home(&env).join(".claude"));
}

#[test]
fn test_tilde_is_expanded() {
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("~/somewhere"));
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), home(&env).join("somewhere"));
}

#[test]
fn test_dirs_defaults_to_single_home() {
    let (_h, env) = clean_env();
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![home(&env).join(".claude")]);
}

#[test]
fn test_dirs_reads_multi_env() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    env.claude_config_dirs = Some(os(format!("{}{}{}", a.display(), pathsep(), b.display())));
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![a, b]);
}

#[test]
fn test_dirs_dedupes_and_skips_blanks() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    env.claude_config_dirs = Some(os(format!("{}{}{}{}", a.display(), pathsep(), pathsep(), a.display())));
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![a]);
}

#[test]
fn test_dirs_all_blank_falls_back() {
    let (_h, mut env) = clean_env();
    env.claude_config_dirs = Some(os(pathsep()));
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![home(&env).join(".claude")]);
}

/// The daemon serves several homes; a stray single-value env must not shrink that.
#[test]
fn test_multi_env_beats_single_env() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    env.claude_config_dir = Some(os(tmp.path().join("single")));
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    env.claude_config_dirs = Some(os(format!("{}{}{}", a.display(), pathsep(), b.display())));
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![a, b]);
}

#[test]
fn test_transcript_roots_appends_projects() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    env.claude_config_dirs = Some(os(format!("{}{}{}", a.display(), pathsep(), b.display())));
    assert_eq!(transcript_roots_in(&env, None).unwrap(), vec![a.join("projects"), b.join("projects")]);
}

/// settings.json, measured not assumed.
///
/// settings.local.json looked safer (wrappers own settings.json), but an A/B test settled it: hooks there
/// alone never fired, and moving them to settings.json worked immediately — Claude Code does not read the local
/// file from a $CLAUDE_CONFIG_DIR home. The era-code collision is fixed on era-code's side (it now preserves
/// third-party hooks) rather than by hiding in a file nothing reads.
#[test]
fn test_hooks_target_the_file_claude_actually_reads() {
    let (_h, env) = clean_env();
    assert_eq!(settings_path_in(&env, None).unwrap().file_name().unwrap(), "settings.json");
}

// ---------------------------------------------------------------------------------------------------------------
// Beyond the Python file: daemon_config_dirs and the pathlib semantics.

#[test]
fn daemon_dirs_union_wrapper_home_with_default() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = tmp.path().join("wrapper");
    env.claude_config_dir = Some(os(&wrapper));
    assert_eq!(daemon_config_dirs_in(&env, None).unwrap(), vec![wrapper, home(&env).join(".claude")]);
}

#[test]
fn daemon_dirs_default_only_once() {
    let (_h, env) = clean_env();
    assert_eq!(daemon_config_dirs_in(&env, None).unwrap(), vec![home(&env).join(".claude")]);
    // The single env naming the default home (trailing slash and all) is still one entry.
    let mut env2 = env.clone();
    env2.claude_config_dir = Some(os(format!("{}/.claude/", home(&env).display())));
    assert_eq!(daemon_config_dirs_in(&env2, None).unwrap(), vec![home(&env).join(".claude")]);
}

#[test]
fn daemon_dirs_multi_env_is_taken_as_is() {
    let (_h, mut env) = clean_env();
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    env.claude_config_dir = Some(os(tmp.path().join("single")));
    env.claude_config_dirs = Some(os(format!(" {}{}{} ", a.display(), pathsep(), b.display())));
    assert_eq!(daemon_config_dirs_in(&env, None).unwrap(), vec![a.clone(), b.clone()]);
    // An explicit override beats the variable.
    let c = tmp.path().join("c");
    assert_eq!(daemon_config_dirs_in(&env, Some(c.as_os_str())).unwrap(), vec![c]);
}

#[test]
fn empty_override_falls_through_to_env() {
    // `override or os.environ.get(...)`: an empty string is falsy.
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("/from/env"));
    assert_eq!(claude_config_dir_in(&env, Some(OsStr::new(""))).unwrap(), PathBuf::from("/from/env"));
    env.claude_config_dirs = Some(os("/x:/y"));
    if PATHSEP == b':' {
        assert_eq!(
            claude_config_dirs_in(&env, Some(OsStr::new(""))).unwrap(),
            vec![PathBuf::from("/x"), PathBuf::from("/y")]
        );
    }
}

#[test]
fn blank_override_does_not_fall_through_to_env() {
    // "   " is truthy, so it is chosen and then stripped to nothing: the default home, not the env value.
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("/from/env"));
    assert_eq!(claude_config_dir_in(&env, Some(OsStr::new("   "))).unwrap(), home(&env).join(".claude"));
}

#[test]
fn fallbacks_ignore_override_but_read_single_env() {
    // An all-blank list falls back to claude_config_dir() with no override, so $CLAUDE_CONFIG_DIR applies.
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("/single"));
    let sep = os(pathsep());
    assert_eq!(claude_config_dirs_in(&env, Some(&sep)).unwrap(), vec![PathBuf::from("/single")]);
}

#[test]
fn paths_are_normalized_like_pathlib() {
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("/a//b/./c/"));
    assert_eq!(claude_config_dir_in(&env, None).unwrap().as_os_str(), "/a/b/c");
    env.claude_config_dir = Some(os("//net/x"));
    assert_eq!(claude_config_dir_in(&env, None).unwrap().as_os_str(), "//net/x");
    env.claude_config_dir = Some(os("rel/./dir/"));
    assert_eq!(settings_path_in(&env, None).unwrap().as_os_str(), "rel/dir/settings.json");
    env.claude_config_dir = Some(os("./"));
    assert_eq!(settings_path_in(&env, None).unwrap().as_os_str(), "settings.json");
}

#[test]
fn dedupe_compares_normalized_paths() {
    let (_h, mut env) = clean_env();
    if PATHSEP != b':' {
        return;
    }
    env.claude_config_dirs = Some(os("/a/:/a:/a/./:~/x:~/x/"));
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![PathBuf::from("/a"), home(&env).join("x")]);
    // pathlib keeps exactly two leading slashes distinct from one.
    env.claude_config_dirs = Some(os("/a://a"));
    assert_eq!(claude_config_dirs_in(&env, None).unwrap(), vec![PathBuf::from("/a"), PathBuf::from("//a")]);
}

#[test]
fn strip_uses_python_whitespace() {
    let (_h, mut env) = clean_env();
    // \x1c is whitespace to str.strip (not to Rust's trim); NBSP and the ideographic space are too.
    env.claude_config_dir = Some(os("\u{1c}\u{a0} /p \u{3000}\t"));
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), PathBuf::from("/p"));
}

#[test]
fn empty_home_is_root() {
    // posixpath.expanduser: an empty $HOME expands "~" to "/".
    let env = HomeEnv { home: Some(os("")), ..HomeEnv::default() };
    assert_eq!(claude_config_dir_in(&env, None).unwrap().as_os_str(), "/.claude");
    let env = HomeEnv { home: Some(os("/")), ..HomeEnv::default() };
    assert_eq!(claude_config_dir_in(&env, None).unwrap().as_os_str(), "/.claude");
    let env = HomeEnv { home: Some(os("/Users/me//")), ..HomeEnv::default() };
    assert_eq!(claude_config_dir_in(&env, None).unwrap().as_os_str(), "/Users/me/.claude");
}

#[test]
fn undeterminable_home_is_an_error() {
    // A $HOME that itself starts with "~" is what pathlib refuses with RuntimeError.
    let env = HomeEnv { home: Some(os("~nobody-here")), ..HomeEnv::default() };
    assert_eq!(claude_config_dir_in(&env, None), Err(HomeDirError));
    assert_eq!(HomeDirError.to_string(), "Could not determine home directory.");
    // An unknown ~user, likewise.
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("~no-such-user-eve-claude-home/x"));
    assert_eq!(claude_config_dir_in(&env, None), Err(HomeDirError));
    // A tilde that is not the first part is left alone.
    env.claude_config_dir = Some(os("/a/~b"));
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), PathBuf::from("/a/~b"));
}

#[cfg(unix)]
#[test]
fn tilde_user_uses_password_database() {
    let (_h, mut env) = clean_env();
    env.claude_config_dir = Some(os("~root/cfg"));
    let got = claude_config_dir_in(&env, None).unwrap();
    let root_home = passwd_home(Some(b"root")).expect("root has a passwd entry");
    assert_eq!(got, path_from_bytes(root_home).join("cfg"));
    // HOME unset: "~" comes from the current user's passwd entry.
    let env = HomeEnv::default();
    let mine = path_from_bytes(passwd_home(None).unwrap());
    assert_eq!(claude_config_dir_in(&env, None).unwrap(), mine.join(".claude"));
}

#[test]
fn process_forms_read_the_process_environment() {
    let env = HomeEnv::from_process();
    assert_eq!(claude_config_dir(None), claude_config_dir_in(&env, None));
    assert_eq!(claude_config_dirs(None), claude_config_dirs_in(&env, None));
    assert_eq!(daemon_config_dirs(None), daemon_config_dirs_in(&env, None));
    assert_eq!(settings_path(None), settings_path_in(&env, None));
    assert_eq!(transcript_roots(None), transcript_roots_in(&env, None));
}
