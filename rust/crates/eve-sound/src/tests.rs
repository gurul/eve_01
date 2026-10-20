//! Port of bridge/tests/test_sound.py, plus the sound.json parts of
//! test_daemon_scene.py.

use super::*;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// A tracing writer that keeps every line, standing in for pytest's caplog.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn with_caplog<T>(f: impl FnOnce() -> T) -> (T, String) {
    let cap = Captured::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt().with_writer(move || writer.clone()).with_ansi(false).finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    (out, cap.text())
}

#[test]
fn test_missing_file_means_sound_on() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = SoundSetting::at(tmp.path().join("sound.json"));
    assert!(s.load() && !s.muted());
}

#[test]
fn test_mute_persists_across_instances() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("cfg").join("sound.json");
    let mut s = SoundSetting::at(&p);
    assert!(s.set(false)); // changed
    assert!(!s.set(false)); // already muted
    let mut again = SoundSetting::at(&p);
    assert!(!again.load() && again.muted());
    again.set(true);
    assert!(SoundSetting::at(&p).load());
}

#[test]
fn test_corrupt_file_keeps_sound_on() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("sound.json");
    std::fs::write(&p, "{not json").unwrap();
    let (on, log) = with_caplog(|| SoundSetting::at(&p).load());
    assert!(on);
    assert!(log.contains("sound: cannot read") && log.contains("sound stays on"), "{log}");
}

#[test]
fn test_non_object_json_does_not_crash_microphone_or_sound_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("switch.json");
    let ((), log) = with_caplog(|| {
        for name in ["mic", "sound"] {
            for text in ["null", "[]", "false", "42", "\"muted\""] {
                std::fs::write(&p, text).unwrap();
                assert!(SoundSetting::new(Some(p.clone()), name).load());
            }
        }
    });
    assert!(log.contains("expected a JSON object"), "{log}");
    assert!(log.contains("mic: cannot read") && log.contains("mic stays on"), "{log}");
    // A valid stored setting is still honored.
    std::fs::write(&p, r#"{"muted": true}"#).unwrap();
    assert!(!SoundSetting::at(&p).load());
}

#[test]
fn test_wire_helpers() {
    assert_eq!(build_sound_cmd(false), json!({"cmd": "sound", "on": false}));
    let page = json!({"cmd": "caption", "page": 0, "chirp": true});
    let page = page.as_object().unwrap();
    let quiet = quiet_caption(page, true);
    assert_eq!(Value::Object(quiet.clone().into_owned()), json!({"cmd": "caption", "page": 0, "chirp": false}));
    // Key order is kept, as Python's {**msg, "chirp": False} keeps it.
    assert_eq!(serde_json::to_string(&*quiet).unwrap(), r#"{"cmd":"caption","page":0,"chirp":false}"#);
    let loud = quiet_caption(page, false);
    assert!(matches!(loud, Cow::Borrowed(m) if std::ptr::eq(m, page)));
    assert_eq!(page["chirp"], json!(true)); // the original is never modified
}

#[test]
fn quiet_caption_follows_python_truthiness() {
    let falsy = json!({"cmd": "caption", "chirp": 0});
    assert!(matches!(quiet_caption(falsy.as_object().unwrap(), true), Cow::Borrowed(_)));
    let absent = json!({"cmd": "caption"});
    assert!(matches!(quiet_caption(absent.as_object().unwrap(), true), Cow::Borrowed(_)));
    let truthy_str = json!({"cmd": "caption", "chirp": "yes"});
    assert_eq!(quiet_caption(truthy_str.as_object().unwrap(), true)["chirp"], json!(false));
}

#[test]
fn test_default_path_env_override() {
    let tmp = tempfile::tempdir().unwrap();
    let x = tmp.path().join("x.json");
    let env = HashMap::from([("EVE_SOUND_FILE".to_string(), x.to_string_lossy().into_owned())]);
    assert_eq!(default_path(Some(&env), "sound"), x);
    assert_eq!(default_path(Some(&HashMap::new()), "sound").file_name().unwrap(), "sound.json");
}

#[test]
fn default_path_names_the_switch_and_strips_and_expands() {
    let empty = HashMap::new();
    let mic = default_path(Some(&empty), "mic");
    assert!(mic.ends_with(".config/eve-bridge/mic.json"), "{}", mic.display());
    let blank = HashMap::from([("EVE_MIC_FILE".to_string(), "   ".to_string())]);
    assert_eq!(default_path(Some(&blank), "mic"), mic);
    let tilde = HashMap::from([("EVE_MIC_FILE".to_string(), " ~/m.json ".to_string())]);
    assert_eq!(default_path(Some(&tilde), "mic"), home().join("m.json"));
    let bare = HashMap::from([("EVE_SOUND_FILE".to_string(), "~".to_string())]);
    assert_eq!(default_path(Some(&bare), "sound"), home());
}

#[test]
fn file_format_matches_python_json_dumps() {
    // test_daemon_scene: json.loads(sound.json) == {"muted": True}, and the
    // bytes are what Python's json.dumps(...) + "\n" writes.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("sound.json");
    let mut s = SoundSetting::at(&p);
    s.set(false);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"muted\": true}\n");
    assert!(!tmp.path().join("sound.tmp").exists());
    s.set(true);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"muted\": false}\n");
}

#[test]
fn muted_value_uses_python_truthiness() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("sound.json");
    for (text, on) in [
        (r#"{}"#, true),
        (r#"{"muted": false}"#, true),
        (r#"{"muted": 0}"#, true),
        (r#"{"muted": ""}"#, true),
        (r#"{"muted": null}"#, true),
        (r#"{"muted": 1}"#, false),
        (r#"{"muted": "no"}"#, false),
        (r#"{"muted": [0]}"#, false),
    ] {
        std::fs::write(&p, text).unwrap();
        assert_eq!(SoundSetting::at(&p).load(), on, "{text}");
    }
}

#[test]
fn unwritable_path_keeps_the_choice_in_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    let mut s = SoundSetting::at(blocker.join("sound.json"));
    let (changed, log) = with_caplog(|| s.set(false));
    assert!(changed && s.muted());
    assert!(log.contains("the choice lasts until the daemon restarts"), "{log}");
}

#[test]
fn directory_in_place_of_file_warns_and_stays_on() {
    let tmp = tempfile::tempdir().unwrap();
    let (on, log) = with_caplog(|| SoundSetting::at(tmp.path()).load());
    assert!(on);
    assert!(log.contains("cannot read"), "{log}");
}
