//! Port of bridge/tests/test_photos.py: naming, the Markdown image line, the
//! two retention caps, and the path guard. No board, no network — bytes and a
//! tmp directory.

use super::*;
use chrono::NaiveDate;
use std::io::Write;
use std::sync::{Arc, Mutex};

const JPEG_HEAD: &[u8] = b"\xff\xd8\xff\xe0";

fn jpeg() -> Vec<u8> {
    let mut v = JPEG_HEAD.to_vec();
    v.extend(std::iter::repeat_n(0u8, 64));
    v.extend_from_slice(b"\xff\xd9");
    v
}

fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, s)
        .unwrap()
}

fn when() -> NaiveDateTime {
    dt(2026, 9, 6, 14, 12, 7)
}

fn cfg(max_photos: Option<i64>, max_bytes: Option<i64>) -> PhotoConfig {
    PhotoConfig {
        max_photos: max_photos.unwrap_or(100),
        max_bytes: max_bytes.unwrap_or(10 * 1024 * 1024),
    }
}

/// caplog: collect the formatted log lines emitted while `f` runs.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn caplog<T>(level: tracing::Level, f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let cap = Capture::default();
    let writer = cap.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let out = tracing::subscriber::with_default(sub, f);
    let text = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    (out, text.lines().map(str::to_string).collect())
}

#[test]
fn test_relative_path_sorts_by_day_then_time() {
    assert_eq!(relative_path(&when(), 42), "photos/2026-09-06/141207-42.jpg");
    let earlier = relative_path(&dt(2026, 9, 6, 9, 5, 0), 7);
    assert!(earlier < relative_path(&when(), 42));
}

#[test]
fn test_photo_line_is_an_indented_image_that_is_not_a_note() {
    let line = photo_line("photos/2026-09-06/141207-42.jpg");
    assert_eq!(line, "  ![](photos/2026-09-06/141207-42.jpg)");
    // Both readers key on a leading "- HH:MM"; this line has neither.
    // notes_widget.LINE_RE, verbatim.
    let line_re = regex::Regex::new(r"^- (\d{1,2}:\d{2}) (.+?)\s*$").unwrap();
    assert!(!line_re.is_match(line.trim()));
    assert!(!line.trim_start().starts_with("- "));
}

#[test]
fn test_save_writes_the_jpeg_private_and_returns_a_relative_path() {
    let tmp = tempfile::tempdir().unwrap();
    let rel = save(tmp.path(), &when(), 42, &jpeg(), Some(cfg(None, None)));
    assert_eq!(rel.as_deref(), Some("photos/2026-09-06/141207-42.jpg"));
    let path = tmp.path().join(rel.unwrap());
    assert_eq!(fs::read(&path).unwrap(), jpeg());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(tmp.path().join("photos")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    assert_eq!(usage(tmp.path()), (1, jpeg().len() as u64));
}

#[test]
fn test_save_refuses_non_jpeg_bytes_and_a_zero_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let (r, logs) = caplog(tracing::Level::WARN, || {
        save(tmp.path(), &when(), 1, b"GIF89a...", Some(cfg(None, None)))
    });
    assert_eq!(r, None);
    assert!(logs.iter().any(|l| l.contains("not a JPEG")));
    assert_eq!(save(tmp.path(), &when(), 1, &jpeg(), Some(cfg(Some(0), None))), None);
    assert_eq!(save(tmp.path(), &when(), 1, &jpeg(), Some(cfg(None, Some(0)))), None);
    assert!(!tmp.path().join("photos").exists());
}

#[test]
fn test_saving_prunes_the_oldest_past_the_count_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(Some(3), None);
    let rels: Vec<String> = (0..5)
        .map(|i| save(tmp.path(), &dt(2026, 9, 6, 10, 0, i), i as i64, &jpeg(), Some(c)).unwrap())
        .collect();
    let mut kept: Vec<String> = all_photos(tmp.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 3);
    let newest: Vec<String> = rels[2..]
        .iter()
        .map(|r| r.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(newest, kept); // the three newest
    assert!(!tmp.path().join(&rels[0]).exists());
}

#[test]
fn test_pruning_past_the_size_cap_ignores_the_count() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(Some(100), Some(jpeg().len() as i64 * 2));
    for i in 0..4 {
        save(tmp.path(), &dt(2026, 9, 6, 10, 0, i), i as i64, &jpeg(), Some(c));
    }
    let (n, total) = usage(tmp.path());
    assert!(n == 2 && total as i64 <= c.max_bytes);
}

#[test]
fn test_pruning_removes_the_day_directory_it_empties() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(Some(1), None);
    save(tmp.path(), &dt(2026, 9, 5, 10, 0, 0), 1, &jpeg(), Some(c));
    save(tmp.path(), &dt(2026, 9, 6, 10, 0, 0), 2, &jpeg(), Some(c));
    assert!(!tmp.path().join("photos").join("2026-09-05").exists());
    assert!(tmp.path().join("photos").join("2026-09-06").is_dir());
}

#[test]
fn test_prune_on_an_empty_shelf_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(prune(tmp.path(), Some(cfg(None, None))).is_empty());
    assert_eq!(usage(tmp.path()), (0, 0));
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_configured_reads_the_env_and_falls_back_on_junk() {
    assert_eq!(configured(Some(&env(&[]))), PhotoConfig::default());
    let c = configured(Some(&env(&[("EVE_PHOTOS_MAX", "12"), ("EVE_PHOTOS_MAX_MB", "2.5")])));
    assert!(c.max_photos == 12 && c.max_bytes == (2.5 * 1024.0 * 1024.0) as i64);
    let (junk, logs) = caplog(tracing::Level::WARN, || {
        configured(Some(&env(&[("EVE_PHOTOS_MAX", "lots"), ("EVE_PHOTOS_MAX_MB", "big")])))
    });
    assert_eq!(junk, PhotoConfig::default());
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().any(|l| l.contains("photos: EVE_PHOTOS_MAX='lots' is not a number; using 300")));
    assert!(logs.iter().any(|l| l.contains("photos: EVE_PHOTOS_MAX_MB='big' is not a number; using 100")));
}

#[test]
fn test_iter_recent_is_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..4 {
        save(tmp.path(), &dt(2026, 9, 6, 10, 0, i), i as i64, &jpeg(), Some(cfg(None, None)));
    }
    let names: Vec<String> = iter_recent(tmp.path(), 2)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["100003-3.jpg", "100002-2.jpg"]);
}

// Beyond the Python file: the parsing edges the port reproduces by hand.

#[test]
fn configured_parses_like_python_int_of_float() {
    let c = configured(Some(&env(&[("EVE_PHOTOS_MAX", " 12.9 "), ("EVE_PHOTOS_MAX_MB", "-3")])));
    assert_eq!(c.max_photos, 12);
    assert_eq!(c.max_bytes, 0);
    let c = configured(Some(&env(&[("EVE_PHOTOS_MAX", "-5"), ("EVE_PHOTOS_MAX_MB", "1_0")])));
    assert_eq!(c.max_photos, 0);
    assert_eq!(c.max_bytes, 10 * 1024 * 1024);
    let c = configured(Some(&env(&[("EVE_PHOTOS_MAX", "1e2"), ("EVE_PHOTOS_MAX_MB", "nan")])));
    assert_eq!(c.max_photos, 100);
    assert_eq!(c.max_bytes, 0);
    let (c, logs) = caplog(tracing::Level::WARN, || {
        configured(Some(&env(&[("EVE_PHOTOS_MAX", "nan"), ("EVE_PHOTOS_MAX_MB", "_1")])))
    });
    assert_eq!(c, PhotoConfig::default());
    assert_eq!(logs.len(), 2);
}

#[test]
fn photo_config_default_matches_python() {
    let d = PhotoConfig::default();
    assert_eq!(d.max_photos, 300);
    assert_eq!(d.max_bytes, 104_857_600);
}

#[test]
fn format_usage_counts_and_megabytes() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(format_usage(tmp.path()), "0 photos, 0.0 MB");
    save(tmp.path(), &when(), 1, &jpeg(), Some(cfg(None, None)));
    assert_eq!(format_usage(tmp.path()), "1 photo, 0.0 MB");
}

#[test]
fn all_photos_ignores_other_files_and_loose_files() {
    let tmp = tempfile::tempdir().unwrap();
    save(tmp.path(), &when(), 1, &jpeg(), Some(cfg(None, None)));
    let day = tmp.path().join("photos/2026-09-06");
    fs::write(day.join("notes.txt"), b"x").unwrap();
    fs::write(day.join(".jpg"), b"x").unwrap();
    fs::write(tmp.path().join("photos/loose.jpg"), b"x").unwrap();
    let names: Vec<String> = all_photos(tmp.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["141207-1.jpg"]);
}

#[test]
fn iter_recent_zero_and_negative_follow_python_slicing() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..4 {
        save(tmp.path(), &dt(2026, 9, 6, 10, 0, i), i as i64, &jpeg(), Some(cfg(None, None)));
    }
    let names = |last: i64| -> Vec<String> {
        iter_recent(tmp.path(), last)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(names(0).len(), 4);
    assert_eq!(names(10).len(), 4);
    assert_eq!(names(-3), vec!["100003-3.jpg"]);
}

#[test]
fn py_repr_quotes_like_python() {
    assert_eq!(py_repr("lots"), "'lots'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a'\"b"), "'a\\'\"b'");
}
