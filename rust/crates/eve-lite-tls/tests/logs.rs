//! test_logs_carry_no_name_and_no_address (bridge/tests/test_lite_tls.py). A scoped tracing subscriber misses
//! events while other test threads register the same callsites, so this test has a process of its own.

use std::io::{self, Write};
use std::sync::Arc;

use eve_lite_tls::LocalTls;

const HOST: &str = "Kitchen-Mac";
const LAN: &str = "192.168.1.20";

#[derive(Clone, Default)]
struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_logs_carry_no_name_and_no_address() {
    let tmp = tempfile::tempdir().unwrap();
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let t = LocalTls::new(tmp.path().join("web-tls"));
        t.refresh([LAN], HOST).unwrap();
        t.refresh(["10.0.0.7"], "Den-Mac").unwrap();
    });
    let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("issued the LAN certificate")); // control: the lines were captured
    for secret_ish in [
        HOST,
        &HOST.to_lowercase(),
        "Den-Mac",
        "den-mac",
        LAN,
        "10.0.0.7",
    ] {
        assert!(!text.contains(secret_ish), "{secret_ish} in the log");
    }
}
