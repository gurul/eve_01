//! test_modes_hold_under_any_umask_and_loose_ones_are_tightened (bridge/tests/test_lite_tls.py). The umask is
//! process-wide, so this test has a process of its own: in the main test binary it would change the modes of
//! files other tests' threads create meanwhile.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use eve_lite_tls::LocalTls;

const HOST: &str = "Kitchen-Mac";
const LAN: &str = "192.168.1.20";

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn umask(mask: libc::mode_t) -> libc::mode_t {
    unsafe { libc::umask(mask) }
}

#[test]
fn test_modes_hold_under_any_umask_and_loose_ones_are_tightened() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web-tls");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    let old = umask(0);
    let t = LocalTls::new(&root);
    let result = t.refresh([LAN], HOST);
    umask(old);
    result.unwrap();
    assert!(
        mode(&root) == 0o700 && mode(&t.ca_key_path) == 0o600 && mode(&t.leaf_key_path) == 0o600
    );
    let old = umask(0o277); // a strict umask would leave keys 0400
    let strict = LocalTls::new(tmp.path().join("strict"));
    let result = strict.refresh([LAN], HOST);
    umask(old);
    result.unwrap();
    assert_eq!(mode(&strict.root), 0o700);
    for entry in fs::read_dir(&strict.root).unwrap() {
        assert_eq!(mode(&entry.unwrap().path()), 0o600);
    }
    fs::set_permissions(&t.leaf_key_path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!t.refresh([LAN], HOST).unwrap()); // nothing to issue, but the modes come back
    assert!(mode(&root) == 0o700 && mode(&t.leaf_key_path) == 0o600);
}
