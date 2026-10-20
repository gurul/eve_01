//! Bounded Telegram image intake and private, temporary files for local relays.
//!
//! Port of `eve_bridge/telegram_images.py`. The Python module asks Pillow two
//! questions about an image: what it is (format, size, frame count, read at
//! `Image.open`) and whether its pixels decode (`verify()` + `load()`). The
//! Rust side answers the first with header walkers in [`probe`] that follow
//! Pillow's own parsers, so a file gets the same one of the three messages the
//! owner sees, and the second with the `image` crate's decoders.

mod probe;

use std::fs;
use std::future::Future;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use regex::Regex;
use serde_json::{json, Value};

pub const MAX_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 25_000_000;
pub const KEEP_SECS: u64 = 24 * 60 * 60;
/// Pillow format name, MIME type and file suffix, in the Python dict's order.
pub const FORMATS: [(&str, &str, &str); 4] = [
    ("JPEG", "image/jpeg", ".jpg"),
    ("PNG", "image/png", ".png"),
    ("WEBP", "image/webp", ".webp"),
    ("GIF", "image/gif", ".gif"),
];

/// Where Telegram serves bot files. [`download_with`] takes another base for a
/// local Bot API server or a test.
pub const TELEGRAM_FILE_BASE: &str = "https://api.telegram.org";

/// How many expired-or-live images [`save`] keeps before it refuses a new one.
const MAX_SAVED: usize = 100;

const TOO_LARGE: &str = "That image is too large. Send an image under 10 MB.";
const NOT_STILL: &str = "Send a still JPEG, PNG, WebP, or GIF image.";
const TOO_MANY_PIXELS: &str = "That image is too large. Send a smaller version under 25 megapixels.";
const UNREADABLE: &str =
    "I couldn't read that image. Send it again as a photo or a still JPEG or PNG.";
const NO_METADATA: &str = "Telegram couldn't provide that image. Please send it again.";
const BAD_PATH: &str = "Telegram returned an invalid image path. Please send it again.";
const NO_DOWNLOAD: &str = "Telegram couldn't download that image. Please send it again.";
const DOWNLOAD_FAILED: &str = "The image download failed. Please send it again.";
const FOLDER_UNAVAILABLE: &str = "The private image folder is unavailable.";
const FOLDER_FULL: &str = "The temporary image folder is full. Try again after older images expire.";

/// Safe to show in chat; never contains a token, URL, or image content.
///
/// [`download`] and [`save`] return `anyhow::Error`; callers that show the
/// message test for this type with `err.downcast_ref::<ImageError>()`, as the
/// Python callers catch `ImageError`. Other errors (the `getFile` call failing,
/// a filesystem error) pass through unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ImageError(pub String);

impl ImageError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

fn fail(message: &str) -> ImageError {
    ImageError(message.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attachment {
    pub file_id: String,
    pub size: u64,
}

impl Attachment {
    /// `Attachment(file_id)`: size defaults to 0 (unknown).
    pub fn new(file_id: impl Into<String>) -> Self {
        Self { file_id: file_id.into(), size: 0 }
    }

    pub fn with_size(file_id: impl Into<String>, size: u64) -> Self {
        Self { file_id: file_id.into(), size }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReceivedImage {
    pub data: Vec<u8>,
    pub mime: String,
    pub suffix: String,
}

impl ReceivedImage {
    pub fn new(data: Vec<u8>, mime: impl Into<String>, suffix: impl Into<String>) -> Self {
        Self { data, mime: mime.into(), suffix: suffix.into() }
    }

    pub fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime,
            base64::engine::general_purpose::STANDARD.encode(&self.data)
        )
    }
}

/// The two module limits the Python tests patch. Production uses
/// [`Limits::default`], which is `MAX_BYTES` and `MAX_PIXELS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_pixels: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_bytes: MAX_BYTES, max_pixels: MAX_PIXELS }
    }
}

/// Python `isinstance(v, int) and not isinstance(v, bool)` for a JSON value.
fn json_int(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

/// The image in a Telegram message: the largest photo size, or an image
/// document. `None` when the message carries no usable image.
pub fn attachment(message: &Value) -> Option<Attachment> {
    let empty = Vec::new();
    let mut candidates: Vec<&Value> = match message.get("photo") {
        Some(Value::Array(photos)) => photos.iter().collect(),
        _ => empty.iter().collect(),
    };
    if let Some(document @ Value::Object(_)) = message.get("document") {
        // str(document.get("mime_type", "")).startswith("image/"): only a string can.
        if document.get("mime_type").and_then(Value::as_str).is_some_and(|m| m.starts_with("image/")) {
            candidates = vec![document];
        }
    }
    let valid: Vec<(String, u64)> = candidates
        .into_iter()
        .filter_map(|p| {
            let p = p.as_object()?;
            let file_id = p.get("file_id")?.as_str().filter(|id| !id.is_empty())?;
            let size = match p.get("file_size") {
                None => 0,
                Some(v) => json_int(v).filter(|s| *s >= 0)?,
            };
            Some((file_id.to_string(), u64::try_from(size).unwrap_or(u64::MAX)))
        })
        .collect();
    // Telegram orders photo sizes from smallest to largest. Prefer measured size when provided.
    let (_, (file_id, size)) = valid
        .into_iter()
        .enumerate()
        .max_by_key(|(index, (_, size))| (*size, *index))?;
    Some(Attachment { file_id, size })
}

/// Check that `data` is one still JPEG, PNG, WebP or GIF image that decodes.
pub fn validate(data: &[u8]) -> Result<ReceivedImage, ImageError> {
    validate_with(data, Limits::default())
}

/// [`validate`] with explicit limits.
pub fn validate_with(data: &[u8], limits: Limits) -> Result<ReceivedImage, ImageError> {
    if data.len() > limits.max_bytes {
        return Err(fail(TOO_LARGE));
    }
    let opened = guarded(|| probe::open(data)).ok_or_else(|| fail(UNREADABLE))?;
    let Some(format) = opened.format else {
        return Err(fail(NOT_STILL));
    };
    if opened.n_frames != 1 {
        return Err(fail(NOT_STILL));
    }
    if u64::from(opened.width) * u64::from(opened.height) > limits.max_pixels {
        return Err(fail(TOO_MANY_PIXELS));
    }
    let (_, mime, suffix) = FORMATS[format as usize];
    // verify() alone does not decode JPEG pixel data.
    guarded(|| probe::verify_and_load(&opened, data)).ok_or_else(|| fail(UNREADABLE))?;
    Ok(ReceivedImage::new(data.to_vec(), mime, suffix))
}

/// Run a probe step; a decoder that panics on hostile input counts as an
/// image that cannot be read, as a Pillow decoder error does.
fn guarded<T>(step: impl FnOnce() -> Result<T, probe::Broken>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).ok()?.ok()
}

static FILE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_/-]+\.[A-Za-z0-9]+$").expect("file path pattern"));

/// The download client: never follows a redirect (the token is in the URL),
/// 30 s to connect and 30 s per read, as httpx's `timeout=30`.
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client")
});

/// Fetch a Telegram attachment, bounded to `MAX_BYTES`, and validate it.
///
/// `get_file` is the Bot API call (`getFile` with `{"file_id": …}`) returning
/// its `result`; its errors pass through. The file request goes out on a
/// client that never follows redirects.
pub async fn download<F, Fut>(get_file: F, token: &str, item: &Attachment) -> anyhow::Result<ReceivedImage>
where
    F: FnOnce(&'static str, Value) -> Fut,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    download_with(&CLIENT, TELEGRAM_FILE_BASE, Limits::default(), get_file, token, item).await
}

/// [`download`] against another file server base and limits. `client` must
/// not follow redirects; [`download`] passes one that does not.
pub async fn download_with<F, Fut>(
    client: &reqwest::Client,
    base: &str,
    limits: Limits,
    get_file: F,
    token: &str,
    item: &Attachment,
) -> anyhow::Result<ReceivedImage>
where
    F: FnOnce(&'static str, Value) -> Fut,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    let max = limits.max_bytes as u64;
    if item.size > max {
        return Err(fail(TOO_LARGE).into());
    }
    let result = get_file("getFile", json!({"file_id": item.file_id})).await?;
    let Value::Object(result) = result else {
        return Err(fail(NO_METADATA).into());
    };
    // isinstance(size, int): a JSON bool is a Python int here.
    let size = match result.get("file_size") {
        None => Some(0),
        Some(Value::Bool(b)) => Some(i128::from(*b)),
        Some(v) => json_int(v),
    };
    if !size.is_some_and(|s| s >= 0 && s <= i128::from(max)) {
        return Err(fail(TOO_LARGE).into());
    }
    let path = match result.get("file_path") {
        None => "",
        Some(Value::String(p)) => p.as_str(),
        Some(_) => return Err(fail(BAD_PATH).into()),
    };
    if !FILE_PATH.is_match(path)
        || path.starts_with('/')
        || path.split('/').any(|p| matches!(p, "." | ".." | ""))
    {
        return Err(fail(BAD_PATH).into());
    }
    let data = fetch(client, &format!("{base}/file/bot{token}/{path}"), max).await?;
    let image = tokio::task::spawn_blocking(move || validate_with(&data, limits)).await??;
    Ok(image)
}

/// GET the file, refusing anything but a 200 and stopping past `max` bytes.
/// Transport errors become one message that names no URL.
async fn fetch(client: &reqwest::Client, url: &str, max: u64) -> Result<Vec<u8>, ImageError> {
    let mut response = client.get(url).send().await.map_err(|_| fail(DOWNLOAD_FAILED))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(fail(NO_DOWNLOAD));
    }
    if let Some(length) = response.headers().get(reqwest::header::CONTENT_LENGTH) {
        let length = String::from_utf8_lossy(length.as_bytes()).into_owned();
        if !length.is_empty()
            && (!length.bytes().all(|b| b.is_ascii_digit())
                || length.parse::<u128>().map_or(true, |n| n > u128::from(max)))
        {
            return Err(fail(TOO_LARGE));
        }
    }
    let mut data = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                data.extend_from_slice(&chunk);
                if data.len() as u64 > max {
                    return Err(fail(TOO_LARGE));
                }
            }
            Ok(None) => return Ok(data),
            Err(_) => return Err(fail(DOWNLOAD_FAILED)),
        }
    }
}

static SAVED_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{32}\.(jpg|png|webp|gif)$").expect("saved name pattern"));

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `Path(tempfile.gettempdir()) / f"eve-telegram-images-{os.getuid()}"`.
pub fn default_root() -> PathBuf {
    gettempdir().join(format!("eve-telegram-images-{}", uid()))
}

/// Retain for delayed relay reads; expire on the next save, bounded to 100 files.
///
/// `root` defaults to [`default_root`]. Returns the file's resolved path.
pub fn save(image: &ReceivedImage, root: Option<&Path>) -> anyhow::Result<PathBuf> {
    let root = root.map_or_else(default_root, Path::to_path_buf);
    if let Err(err) = fs::DirBuilder::new().mode(0o700).create(&root) {
        // Path.mkdir(exist_ok=True) accepts an existing directory, a symlink to one included.
        if err.kind() != ErrorKind::AlreadyExists || !root.is_dir() {
            return Err(err.into());
        }
    }
    let info = fs::symlink_metadata(&root)?;
    if !info.file_type().is_dir() || info.uid() != uid() || info.mode() & 0o077 != 0 {
        return Err(fail(FOLDER_UNAVAILABLE).into());
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let mut remaining = 0usize;
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        let info = fs::symlink_metadata(&path)?;
        let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| SAVED_NAME.is_match(n));
        if named && info.file_type().is_file() {
            let mtime = info.mtime() as f64 + info.mtime_nsec() as f64 / 1e9;
            if now - mtime > KEEP_SECS as f64 {
                fs::remove_file(&path)?;
            } else {
                remaining += 1;
            }
        }
    }
    if remaining >= MAX_SAVED {
        return Err(fail(FOLDER_FULL).into());
    }
    let path = root.join(format!("{}{}", uuid::Uuid::new_v4().simple(), image.suffix));
    let mut out = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    out.write_all(&image.data)?;
    drop(out);
    Ok(fs::canonicalize(&path)?)
}

/// Python's `tempfile.gettempdir()`: the first of `$TMPDIR`, `$TEMP`, `$TMP`,
/// `/tmp`, `/var/tmp`, `/usr/tmp` and the working directory where a file can
/// be created, made absolute and normalised, chosen once per process.
fn gettempdir() -> PathBuf {
    static TEMPDIR: OnceLock<PathBuf> = OnceLock::new();
    TEMPDIR
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = ["TMPDIR", "TEMP", "TMP"]
                .iter()
                .filter_map(|name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from))
                .collect();
            candidates.extend(["/tmp", "/var/tmp", "/usr/tmp"].map(PathBuf::from));
            candidates.push(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            candidates
                .into_iter()
                .map(|dir| abspath(&dir))
                .find(|dir| writable(dir))
                // Python raises FileNotFoundError here; the save that follows fails the same way.
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
        .clone()
}

/// `os.path.abspath`: join to the working directory and normalise lexically.
fn abspath(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::from("/");
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(p) => out.push(p),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Python's probe: create, write and remove a random file; try a few names.
fn writable(dir: &Path) -> bool {
    for _ in 0..100 {
        let probe = dir.join(format!("tmp{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&probe) {
            Ok(mut file) => {
                let wrote = file.write_all(b"blat").is_ok();
                drop(file);
                let removed = fs::remove_file(&probe).is_ok();
                return wrote && removed;
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests;
