use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;

const JPEG_HEAD: &[u8] = b"\xff\xd8\xff\xe0fake-jpeg";

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A fake ffmpeg: a shell script with `body`, executable.
fn fake_ffmpeg(dir: &Path, body: &str) -> String {
    let path = dir.join("ffmpeg");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

// ---- ported from bridge/tests/test_photo_request.py ----------------------------------------------------------

#[test]
fn test_the_camera_command_reads_one_settled_frame_without_a_microphone() {
    let cmd = command("/x/ffmpeg", "default");
    let at = |flag: &str| cmd[cmd.iter().position(|a| a == flag).unwrap() + 1].clone();
    assert!(cmd[0] == "/x/ffmpeg" && at("-i") == "default:none");
    assert!(at("-vf").contains(&format!("gte(n\\,{WARMUP_FRAMES})")));
    assert!(at("-frames:v") == "1" && cmd.last().unwrap() == "pipe:1");
    assert_eq!(device(Some(&env(&[]))).as_deref(), Some("default"));
    assert_eq!(device(Some(&env(&[("EVE_MAC_CAMERA", "2")]))).as_deref(), Some("2"));
    assert_eq!(device(Some(&env(&[("EVE_MAC_CAMERA", "off")]))), None);
}

#[tokio::test]
async fn test_a_switched_off_camera_never_starts_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let ffmpeg = fake_ffmpeg(dir.path(), &format!("touch '{}'", marker.display()));
    let mut looked = false;
    let r = snap_with(Some(&env(&[("EVE_MAC_CAMERA", "off")])), TIMEOUT_SECS, || {
        looked = true;
        Some(ffmpeg)
    })
    .await;
    assert!(!r.ok() && r.reason().unwrap().contains("switched off"));
    assert!(!looked && !marker.exists());
}

// ---- the rest of mac_camera's behavior ------------------------------------------------------------------------

#[test]
fn the_full_command_is_pythons_argv() {
    assert_eq!(
        command("/opt/homebrew/bin/ffmpeg", "OBSBOT Tiny 3"),
        vec![
            "/opt/homebrew/bin/ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin",
            "-f", "avfoundation", "-pixel_format", "nv12", "-framerate", "30", "-i", "OBSBOT Tiny 3:none",
            "-vf", "select=gte(n\\,15),scale='min(1920,iw)':-2",
            "-frames:v", "1", "-q:v", "3", "-f", "image2pipe", "-vcodec", "mjpeg", "pipe:1",
        ]
    );
}

#[test]
fn every_off_word_turns_the_camera_off_in_any_case() {
    for word in ["off", "OFF", " Off ", "0", "false", "False", "no", "NO", "none", "None"] {
        assert_eq!(device(Some(&env(&[(ENV, word)]))), None, "{word:?}");
    }
}

#[test]
fn a_blank_value_is_the_default_camera_and_names_are_trimmed() {
    assert_eq!(device(Some(&env(&[(ENV, "")]))).as_deref(), Some(DEFAULT_DEVICE));
    assert_eq!(device(Some(&env(&[(ENV, "   ")]))).as_deref(), Some(DEFAULT_DEVICE));
    assert_eq!(
        device(Some(&env(&[(ENV, "  FaceTime HD Camera \n")]))).as_deref(),
        Some("FaceTime HD Camera")
    );
    assert_eq!(device(Some(&env(&[(ENV, "1")]))).as_deref(), Some("1"));
}

#[test]
fn the_fallback_paths_are_the_homebrew_ones() {
    assert_eq!(FFMPEG_PATHS, ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]);
    // Whatever is found is an executable file or nothing.
    if let Some(p) = ffmpeg_path() {
        assert!(executable(&p));
    }
}

#[tokio::test]
async fn no_ffmpeg_is_its_reason() {
    let r = snap_with(Some(&env(&[])), TIMEOUT_SECS, || None).await;
    assert_eq!(r, Snap::Failed { reason: REASON_NO_FFMPEG.into() });
}

#[tokio::test]
async fn a_jpeg_on_stdout_is_the_picture_and_ffmpeg_gets_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let args = dir.path().join("args");
    let body = format!(
        "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nprintf '\\377\\330\\377\\340fake-jpeg'",
        args.display()
    );
    let ffmpeg = fake_ffmpeg(dir.path(), &body);
    let r = snap_with(Some(&env(&[(ENV, "2")])), TIMEOUT_SECS, || Some(ffmpeg.clone())).await;
    assert_eq!(r, Snap::Ok { jpeg: JPEG_HEAD.to_vec() });
    assert!(r.ok() && r.reason().is_none() && r.jpeg() == Some(JPEG_HEAD));
    let got: Vec<String> = std::fs::read_to_string(&args).unwrap().lines().map(String::from).collect();
    assert_eq!(got, command(&ffmpeg, "2")[1..].to_vec());
}

#[tokio::test]
async fn output_that_is_not_a_jpeg_is_no_picture() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(dir.path(), "printf 'PNG?'");
    let r = snap_with(Some(&env(&[])), TIMEOUT_SECS, || Some(ffmpeg)).await;
    assert_eq!(r, Snap::Failed { reason: REASON_NO_PICTURE.into() });
}

#[tokio::test]
async fn a_failed_ffmpeg_is_no_picture_even_with_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(
        dir.path(),
        "printf '\\377\\330partial'\necho 'first' >&2\necho 'Could not lock device for configuration' >&2\nexit 1",
    );
    let r = snap_with(Some(&env(&[])), TIMEOUT_SECS, || Some(ffmpeg)).await;
    assert!(!r.ok() && r.jpeg().is_none());
    assert_eq!(r.reason(), Some(REASON_NO_PICTURE));
}

#[tokio::test]
async fn a_camera_that_hangs_is_killed_and_answers_in_time() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(dir.path(), "exec sleep 30");
    let started = Instant::now();
    let r = snap_with(Some(&env(&[])), 0.3, || Some(ffmpeg)).await;
    assert_eq!(r, Snap::Failed { reason: REASON_TIMEOUT.into() });
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn an_ffmpeg_that_cannot_start_is_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ffmpeg");
    std::fs::write(&path, "not runnable").unwrap(); // not executable
    let ffmpeg = path.to_string_lossy().into_owned();
    let r = snap_with(Some(&env(&[])), TIMEOUT_SECS, || Some(ffmpeg)).await;
    assert_eq!(r, Snap::Failed { reason: REASON_NOT_STARTED.into() });
}

#[test]
fn the_reasons_are_pythons() {
    assert_eq!(REASON_OFF, "the Mac's camera is switched off for eve");
    assert_eq!(REASON_NO_FFMPEG, "ffmpeg is not installed, so eve cannot read the Mac's camera");
    assert_eq!(REASON_NOT_STARTED, "the Mac's camera could not be started");
    assert_eq!(REASON_TIMEOUT, "the Mac's camera did not answer in time");
    assert_eq!(
        REASON_NO_PICTURE,
        "the Mac's camera gave no picture (is another app using it, or is camera access off for eve?)"
    );
}

#[test]
fn the_logged_stderr_is_its_last_line() {
    assert_eq!(stderr_tail(b""), "");
    assert_eq!(stderr_tail(b"  \n "), "");
    assert_eq!(stderr_tail(b"a\nb\r\nlast one\n\n"), "last one");
    assert_eq!(stderr_tail(b"x\rlast"), "last");
    assert_eq!(stderr_tail(b"bad \xff byte"), "bad \u{fffd} byte");
    assert_eq!(py_splitlines("a\u{2028}b\x0cc"), vec!["a", "b", "c"]);
}

#[test]
fn a_signal_is_a_negative_returncode() {
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(returncode(std::process::ExitStatus::from_raw(9)), Some(-9));
    assert_eq!(returncode(std::process::ExitStatus::from_raw(1 << 8)), Some(1));
}
