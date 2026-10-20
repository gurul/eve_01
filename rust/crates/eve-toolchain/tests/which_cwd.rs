//! test_which_follows_subprocess_rules, the part that changes the working folder: alone in its own test binary, so
//! no other test in the process sees the change.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;

#[test]
fn test_which_reads_an_empty_path_entry_as_the_working_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let tool = b.join("tool");
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_current_dir(&b).unwrap();
    let env: HashMap<String, String> = [("PATH".to_string(), format!("{}:", a.display()))].into_iter().collect();
    assert_eq!(eve_toolchain::which("tool", Some(&env)).as_deref(), Some("tool")); // an empty entry is the working folder
    let empty: HashMap<String, String> = [("PATH".to_string(), String::new())].into_iter().collect();
    assert_eq!(eve_toolchain::which("tool", Some(&empty)).as_deref(), Some("tool")); // PATH="" is one empty entry
    let nothing: HashMap<String, String> = [("PATH".to_string(), a.display().to_string())].into_iter().collect();
    assert_eq!(eve_toolchain::which("tool", Some(&nothing)), None);
}
