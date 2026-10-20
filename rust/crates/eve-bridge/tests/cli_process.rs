//! The command line as a process: what cli.main decides before any command runs (test_cli_lite.py's subprocess
//! cases). Each run has a temporary HOME, an env file of its own or none, and a socket path that cannot answer.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_eve-bridge");

fn run(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("EVE_BRIDGE_SOCK", home.join("no.sock"))
        .env("EVE_ENV_FILE", home.join("no-env"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run eve-bridge")
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn test_an_unknown_profile_stops_with_ex_config() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &[("EVE_PROFILE", "lightweight")], &["status"]);
    assert_eq!(o.status.code(), Some(78), "{}", err(&o));
    assert!(err(&o).contains("lightweight"));
}

#[test]
fn test_an_unknown_profile_in_the_env_file_stops_with_ex_config() {
    let home = tempfile::tempdir().unwrap();
    let env = home.path().join("env");
    std::fs::write(&env, "EVE_PROFILE=lightweight\n").unwrap();
    let o = run(home.path(), &[("EVE_ENV_FILE", env.to_str().unwrap())], &["status"]);
    assert_eq!(o.status.code(), Some(78), "{}", err(&o));
    // control: the same file naming lite gets as far as the command
    std::fs::write(&env, "EVE_PROFILE=lite\n").unwrap();
    let o = run(home.path(), &[("EVE_ENV_FILE", env.to_str().unwrap())], &["web", "devices"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
}

#[test]
fn test_each_profile_refuses_the_others_commands() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &[("EVE_PROFILE", "lite")], &["celebrate"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("invalid choice: 'celebrate'"), "{}", err(&o));
    let o = run(home.path(), &[], &["doctor"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("invalid choice: 'doctor'"), "{}", err(&o));
}

#[test]
fn test_mcp_runs_before_the_env_file_is_read() {
    let home = tempfile::tempdir().unwrap();
    // an env file that would stop the CLI if it were read: mcp never reads it
    let env = home.path().join("env");
    std::fs::write(&env, "EVE_PROFILE=lightweight\n").unwrap();
    let mut child = Command::new(BIN)
        .arg("mcp")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.path())
        .env("EVE_BRIDGE_SOCK", home.path().join("no.sock"))
        .env("EVE_ENV_FILE", &env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let o = child.wait().unwrap();
    assert_ne!(o.code(), Some(78)); // the server ended at end of input, not at the profile
    let o = run(home.path(), &[("EVE_ENV_FILE", env.to_str().unwrap())], &["status"]);
    assert_eq!(o.status.code(), Some(78)); // control: any other command reads it
}

#[test]
fn test_version_and_help_exit_zero() {
    let home = tempfile::tempdir().unwrap();
    for (profile, words) in [("full", &["--version"][..]), ("full", &["--help"]), ("lite", &["--help"])] {
        let o = run(home.path(), &[("EVE_PROFILE", profile)], words);
        assert_eq!(o.status.code(), Some(0), "{profile} {words:?}");
    }
}
