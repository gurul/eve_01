//! A port of `share.py`: share a file of the owner's with someone, by iMessage (Messages on this Mac) or by email
//! (a Gmail attachment, or a Google Drive share).
//!
//! The owner asked on 2026-10-04: "cant file sending be optimized through cli … instead of computer use". Until
//! then "send the report to Ana" was a computer task: Codex drove Messages or Mail on the screen, which took
//! minutes. This module does it with two command-line calls and one HTTP round trip, in seconds.
//!
//! Two tools for the chat's brain:
//!
//! * `find_contact` {name}: the people in the owner's Contacts whose name contains the words, with their phone
//!   numbers and email addresses (read only, no approval).
//! * `share_file` {path, to, via, message}: `via` is `imessage` or `email`; `to` is a contact's name, a phone
//!   number or an email address; `message` is an optional short note.
//!
//! **Consent.** `handle("share_file", ...)` never sends. It checks the file and the recipient and returns
//! `{"needs_approval": true, "question": ..., "token": ...}`. The send happens only through `confirm(token)`, and
//! only while the file is the one the question showed: the same real path, inode, size, mtime and SHA-256, read
//! through a descriptor that never follows a link. What is sent is a copy made from that descriptor, never the
//! path opened again. The recipient's exact handle is frozen in the token. A token is good once, for
//! `APPROVAL_TTL_SECS`; a denied, unknown or expired token runs nothing. `ask_and_share(args, ask)` is the whole
//! flow for a caller with a yes/no prompt (telegram's `_ask_user`).
//!
//! **iMessage.** The recipient is resolved with Contacts through JXA (`osascript -l JavaScript`, JSON out); a raw
//! phone number or email address skips the lookup. The send is an AppleScript to Messages: the note (if any), then
//! the file. Every value travels as an `on run argv` argument and the script itself comes in on stdin, so no value
//! is ever part of a script's text (no injection) and none starts with a dash where osascript would read it as an
//! option. osascript's exit code says Messages took the message, not that it was delivered: the answer says so.
//!
//! Messages is sandboxed. On macOS 15 and later a file sent by AppleScript from outside the folders it may read
//! shows "Not Delivered" while osascript exits 0; text still goes. ~/Library/Messages is behind Full Disk Access
//! for every other process, so eve copies the file into a folder of its own, `~/Pictures/eve-outbox`, and sends the
//! copy. Copies older than `OUTBOX_KEEP_SECS` are swept on each send.
//!
//! **Email, attached** (owner, 2026-10-09: "the chat path should be able to send files"). When Gmail may send, the
//! approved bytes are staged exactly as for iMessage, built into one RFC 2822 message and posted to Gmail's media
//! upload send. Gmail delivers 25 MB at most and base64 adds a third, so a file over `MAX_GMAIL_ATTACH_BYTES` goes
//! as a Drive link instead; the question says which.
//!
//! **Email, as a link.** The file is uploaded to the owner's Drive (multipart up to 5 MB, resumable above) into a
//! "eve shared" folder eve made, then given one reader permission with `sendNotificationEmail=true` and the note
//! as `emailMessage`, so Google emails the recipient a link from the owner's account.
//!
//! `EVE_SHARE=1` turns it on (default off). iMessage needs macOS; email needs the Google sign-in of the direct
//! apps with Drive's write scope.

use std::ffi::{CString, OsStr};
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Component;

use base64::Engine as _;
use futures::future::BoxFuture;
use futures::FutureExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[allow(unused_imports)]
use crate::prelude::*;

use eve_transcripts::py;

// py: share.py:79 `log`: tracing, under the target `eve_brain::share`. Logs carry sizes, counts and exit codes only:
// never a file name, a recipient or a note.

/// py: share.py:81 `OFF_REASON`
pub const OFF_REASON: &str = "Sharing files by iMessage or email is off on this computer (EVE_SHARE=1 turns it on)";

/// py: share.py:82 `TOOL_NAMES`
pub const TOOL_NAMES: &[&str] = &["share_file", "find_contact"];

/// py: share.py:83 `VIAS`
pub const VIAS: &[&str] = &["imessage", "email"];

/// py: share.py:85 `APPROVAL_TTL_SECS`
pub const APPROVAL_TTL_SECS: f64 = 600.0;

/// py: share.py:86 `CONTACTS_SECS`
/// the first Contacts read of a session launches Contacts
pub const CONTACTS_SECS: f64 = 25.0;

/// py: share.py:87 `SEND_SECS`
pub const SEND_SECS: f64 = 30.0;

/// py: share.py:88 `DRIVE_SECS`
pub const DRIVE_SECS: f64 = 120.0;

/// py: share.py:89 `MAX_IMESSAGE_BYTES`
pub const MAX_IMESSAGE_BYTES: usize = 104857600;

/// py: share.py:90 `MAX_DRIVE_BYTES`
/// a cap of eve's, not Drive's: an upload this big is a mistake
pub const MAX_DRIVE_BYTES: usize = 1073741824;

/// py: share.py:91 `MULTIPART_MAX`
/// Drive: multipart is for 5 MB or less, resumable above
pub const MULTIPART_MAX: usize = 5242880;

/// py: share.py:92 `MAX_GMAIL_ATTACH_BYTES`
/// Gmail delivers 25 MB at most; base64 makes 18 MB about 24.6 MB
pub const MAX_GMAIL_ATTACH_BYTES: usize = 18874368;

/// py: share.py:93 `GMAIL_SEND_UPLOAD`
pub const GMAIL_SEND_UPLOAD: &str = "https://gmail.googleapis.com/upload/gmail/v1/users/me/messages/send";

/// py: share.py:94 `GMAIL_SEND_SCOPE`
pub const GMAIL_SEND_SCOPE: &str = "https://www.googleapis.com/auth/gmail.send";

/// py: share.py:95 `CHUNK`
pub const CHUNK: i64 = 1048576;

/// py: share.py:96 `MAX_MATCHES`
pub const MAX_MATCHES: usize = 8;

/// py: share.py:97 `MAX_NOTE`
pub const MAX_NOTE: usize = 500;

/// py: share.py:98 `OUTBOX`
/// Messages is sandboxed: it sends a file from here (see the module's notes); expanded at the send.
pub const OUTBOX: &str = "~/Pictures/eve-outbox";

/// py: share.py:99 `OUTBOX_KEEP_SECS`
pub const OUTBOX_KEEP_SECS: f64 = 3600.0;

/// py: share.py:100 `FOLDER_NAME`
pub const FOLDER_NAME: &str = "eve shared";

/// py: share.py:101 `FOLDER_MIME`
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// py: share.py:102 `FOLDER_PROP`
/// appProperties marking the folder eve made
pub const FOLDER_PROP: &[&str] = &["eve", "share-folder"];

/// py: share.py:103 `DRIVE`
pub const DRIVE: &str = "https://www.googleapis.com/drive/v3";

/// py: share.py:104 `DRIVE_UPLOAD`
pub const DRIVE_UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";

/// py: share.py:105 `FILE_FIELDS`
pub const FILE_FIELDS: &str = "id,name,size,webViewLink";

/// py: share.py:106 `RC_TIMEOUT`
pub const RC_TIMEOUT: i64 = 124;

/// py: share.py:107 `RC_NO_START`
pub const RC_NO_START: i64 = 127;

// py: share.py:110 `Runner` and share.py:111 `Ask`: the types at the end of this file.

/// py: share.py:116 `CONTACTS_JXA`
pub const CONTACTS_JXA: &str = "function run(argv) {
  var q = argv[0];
  var max = parseInt(argv[1], 10);
  var people = Application(\"Contacts\").people.whose({name: {_contains: q}})();
  var out = [];
  for (var i = 0; i < people.length && i < max; i++) {
    var p = people[i];
    var phones = p.phones(), emails = p.emails();
    out.push({
      name: p.name(),
      phones: phones.map(function (x) { return {label: x.label(), value: x.value()}; }),
      emails: emails.map(function (x) { return {label: x.label(), value: x.value()}; })
    });
  }
  return JSON.stringify({count: people.length, people: out});
}
";

/// py: share.py:134 `MESSAGES_SEND`
pub const MESSAGES_SEND: &str = "on run argv
  set theHandle to item 1 of argv
  set theFile to POSIX file (item 2 of argv)
  set theNote to item 3 of argv
  tell application \"Messages\"
    set theService to 1st account whose service type = iMessage
    set theEve to participant theHandle of theService
    if theNote is not \"\" then send theNote to theEve
    send theFile to theEve
  end tell
end run
";

/// py: share.py:148 `async def osascript_runner(argv: list[str], stdin: bytes, timeout: float) -> tuple[int, str, str]:`
///
/// The real runner: one process, an argv list (never a shell), the script on stdin, a timeout. A timeout is an
/// answer (RC_TIMEOUT), never an error; a process that cannot start is RC_NO_START.
pub async fn osascript_runner(argv: &[String], stdin: &[u8], timeout: f64) -> (i64, String, String) {
    let Some((program, args)) = argv.split_first() else {
        return (RC_NO_START, String::new(), "could not start (no program)".into());
    };
    let spawned = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // a caller that stops waiting (Python's BaseException) never leaves the process behind
        .kill_on_drop(true)
        .spawn();
    let mut proc = match spawned {
        Ok(proc) => proc,
        Err(e) => return (RC_NO_START, String::new(), format!("could not start ({})", strerror(&e))),
    };
    let stdin_pipe = proc.stdin.take();
    let stdout_pipe = proc.stdout.take();
    let stderr_pipe = proc.stderr.take();
    let communicate = async {
        let write = async move {
            if let Some(mut pipe) = stdin_pipe {
                let _ = pipe.write_all(stdin).await;
                let _ = pipe.shutdown().await;
            }
        };
        let read_out = async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            buf
        };
        let read_err = async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            buf
        };
        let ((), out, err) = tokio::join!(write, read_out, read_err);
        (proc.wait().await, out, err)
    };
    let waited = tokio::time::timeout(std::time::Duration::from_secs_f64(timeout.max(0.0)), communicate).await;
    let (status, out, err) = match waited {
        Ok(done) => done,
        Err(_) => {
            kill(&mut proc);
            return (RC_TIMEOUT, String::new(), String::new());
        }
    };
    let rc = match status {
        Ok(status) => exit_code(&status),
        Err(_) => 0,
    };
    (rc, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
}

/// `proc.returncode or 0`: the exit code, or minus the signal that ended it (asyncio's convention).
fn exit_code(status: &std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code as i64,
        (None, Some(signal)) => -(signal as i64),
        (None, None) => 0,
    }
}

/// `e.strerror or e`: the OS's words without Rust's " (os error N)".
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// The real runner as a [`Runner`].
pub fn osascript() -> Runner {
    Arc::new(|argv: Vec<String>, stdin: Vec<u8>, timeout: f64| {
        async move { osascript_runner(&argv, &stdin, timeout).await }.boxed()
    })
}

/// py: share.py:166 `def _kill(proc: asyncio.subprocess.Process) -> None:`
pub(crate) fn kill(proc: &mut tokio::process::Child) {
    // a process already gone (ProcessLookupError) is fine
    let _ = proc.start_kill();
}

// ---- small rules --------------------------------------------------------------------------------------------

/// py: share.py:175 `_EMAIL`
/// Python flags 32. Python's `\s` also takes \x1c-\x1f (str.isspace), which Rust's does not: added to each class.
pub(crate) static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[^@\s\x1c-\x1f]+@[^@\s\x1c-\x1f]+\.[^@\s\x1c-\x1f]+(?:\n?\z)"#).expect("EMAIL"));

/// py: share.py:176 `_PHONE`
/// Python flags 32. `\s` with \x1c-\x1f, as Python reads it.
pub(crate) static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\+?[\d\s\x1c-\x1f().\-]{7,20}(?:\n?\z)"#).expect("PHONE"));

/// py: share.py:177 `_LABEL`
/// Python flags 32.
pub(crate) static LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^_\$!<(.*)>!\$_(?:\n?\z)"#).expect("LABEL"));

/// py: share.py:178 `_CONTROL`
/// Python flags 32.
pub(crate) static CONTROL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\x00-\x08\x0b-\x1f\x7f]"#).expect("CONTROL"));


/// py: share.py:181 `def is_email(text: str) -> bool:`
pub fn is_email(text: &str) -> bool {
    EMAIL.is_match(py::strip(text))
}

/// py: share.py:185 `def is_phone(text: str) -> bool:`
pub fn is_phone(text: &str) -> bool {
    let t = py::strip(text);
    PHONE.is_match(t) && t.chars().filter(|c| is_digit(*c)).count() >= 7
}

/// `str.isdigit()` for the characters a phone number may hold (`\d`: any Unicode decimal digit).
fn is_digit(c: char) -> bool {
    static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("DIGIT"));
    c.is_ascii_digit() || (!c.is_ascii() && DIGIT.is_match(c.encode_utf8(&mut [0u8; 4])))
}

/// py: share.py:190 `def phone_handle(text: str) -> str:`
///
/// "+1 (555) 010-0199" → "+15550100199": what Messages takes as a handle.
pub fn phone_handle(text: &str) -> String {
    let t = py::strip(text);
    let sign = if t.starts_with('+') { "+" } else { "" };
    format!("{sign}{}", t.chars().filter(|c| is_digit(*c)).collect::<String>())
}

/// py: share.py:196 `def _label(raw: Any) -> str:`
///
/// Contacts' built-in labels ("_$!<Mobile>!$_") cleaned to "mobile".
pub(crate) fn label(raw: &Value) -> String {
    let text = str_or_empty(Some(raw));
    let inner = match LABEL.captures(&text) {
        Some(m) => m.get(1).map(|g| g.as_str().to_string()).unwrap_or_default(),
        None => text.clone(),
    };
    py::strip(&inner).to_lowercase()
}

/// py: share.py:202 `def human_size(n: int) -> str:`
pub fn human_size(n: i64) -> String {
    if n < 1024 {
        return format!("{n} bytes");
    }
    if n < 1024 * 1024 {
        return format!("{:.0} KB", n as f64 / 1024.0);
    }
    format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
}

/// py: share.py:210 `def resolve_file(raw: str, home: Optional[Path] = None) -> tuple[Optional[Path], str]:`
///
/// The owner-path rules of telegram.send_file: a regular file in the owner's home, no hidden folder.
pub fn resolve_file(raw: &str, home: Option<&Path>) -> (Option<PathBuf>, String) {
    let (real, why) = crate::telegram::resolve_owner_path(raw, home);
    let Some(real) = real else {
        return (None, why);
    };
    if real.is_dir() {
        return (None, "that is a folder; name a file in it (list_files shows them)".into());
    }
    if !real.is_file() {
        return (None, "not a regular file".into());
    }
    (Some(real), String::new())
}

/// py: share.py:225 `class ShareConfig:`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareConfig {
    pub enabled: bool,
    pub imessage: bool,
}

/// Python's `sys.platform` on this build: "darwin" on a Mac.
pub const PLATFORM: &str = if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS };

/// py: share.py:230 `def configured(environ: Optional[Mapping[str, str]] = None, platform: str = sys.platform) -> ShareConfig:`
///
/// `platform` is Python's `sys.platform` ([`PLATFORM`] for this computer).
pub fn configured(environ: Option<&Environ>, platform: &str) -> ShareConfig {
    let raw = env_get(environ, "EVE_SHARE").unwrap_or_default();
    let on = matches!(py::strip(&raw).to_lowercase().as_str(), "1" | "true" | "yes" | "on");
    ShareConfig { enabled: on, imessage: on && platform == "darwin" }
}

// ---- tools --------------------------------------------------------------------------------------------------

/// py: share.py:238 `EMAIL_AS_LINK`
pub const EMAIL_AS_LINK: &str = "by email as a Google Drive link Google emails them";

/// py: share.py:239 `EMAIL_ATTACHED`
pub const EMAIL_ATTACHED: &str = "by email, attached, from the owner's Gmail (a file over 18 MB goes as a Google Drive link)";

/// py: share.py:240 `VIA_EMAIL_LINK`
pub const VIA_EMAIL_LINK: &str = "via email to send a Google Drive link that Google emails them";

/// py: share.py:241 `VIA_EMAIL_ATTACHED`
pub const VIA_EMAIL_ATTACHED: &str = "via email to email it as an attachment from the owner's Gmail";

/// py: share.py:243 `TOOLS`
/// The Python value as JSON (tuples are arrays).
pub static TOOLS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"[{"type": "function", "name": "share_file", "strict": true, "description": "Send one of the owner's files to someone: by iMessage, or by email as a Google Drive link Google emails them. The owner is asked yes/no first; you don't ask. `to` is a contact's name, a phone number or an email address. If two people match, you get them back: ask the owner which, then call again with that person's number or address.", "parameters": {"type": "object", "additionalProperties": false, "required": ["path", "to", "via", "message"], "properties": {"path": {"type": "string", "description": "The file's path in the owner's home (~/Documents/report.pdf)."}, "to": {"type": "string", "description": "A name from the owner's Contacts, a phone number or an email address."}, "via": {"type": "string", "enum": ["imessage", "email"]}, "message": {"type": ["string", "null"], "description": "A short note sent with the file; or null."}}}}, {"type": "function", "name": "find_contact", "strict": true, "description": "Look a person up in the owner's Contacts: everyone whose name contains the words, with their phone numbers and email addresses. Read only.", "parameters": {"type": "object", "additionalProperties": false, "required": ["name"], "properties": {"name": {"type": "string"}}}}]"#).expect("TOOLS"));

/// py: share.py:265 `INSTRUCTIONS`
pub const INSTRUCTIONS: &str = "Sharing a file with someone (share_file, find_contact).
To send one of the owner's files to another person, use share_file: via imessage for a phone number or an
iMessage contact, via email to send a Google Drive link that Google emails them. Do not start a computer task for
this. The owner is asked yes/no by eve before anything is sent; do not ask them first yourself.
If share_file returns matches (two people, or one person with several numbers), ask the owner which one and call
again with that number or address. find_contact looks someone up when you only need their details.
An iMessage result means Messages took it, not that it was delivered: say so. An email result has a link: give it.
\"Send the report to Ana\" is share_file; \"send me the report\" is send_file, to the owner in this chat.";


/// py: share.py:276 `class Pending:`
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub token: String,
    pub via: String,
    /// the real path the question named (resolve_file)
    pub path: PathBuf,
    pub size: i64,
    pub mtime_ns: i64,
    /// iMessage: a phone number or an iMessage email; email: the address
    pub handle: String,
    /// the name the owner sees
    pub who: String,
    pub note: String,
    pub created: f64,
    /// the path as the model wrote it, resolved again at the send
    pub raw: String,
    /// email: a Gmail attachment (True) or a Drive link (False), as the question said
    pub attach: bool,
    pub ino: i64,
    pub dev: i64,
    pub sha256: String,
}

/// py: share.py:293 `class FileChanged(Exception):`
///
/// The file at the send is not the one the owner approved.
/// (Python bases: Exception.)
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct FileChanged {
    /// `str(error)`.
    pub message: String,
}

impl FileChanged {
    pub fn new(message: &str) -> Self {
        Self { message: message.to_string() }
    }
}

/// What reading or staging the approved file raised: [`FileChanged`], or an `OSError`.
#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error(transparent)]
    Changed(#[from] FileChanged),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl StageError {
    /// `type(e).__name__`, for a log line.
    pub fn type_name(&self) -> &'static str {
        match self {
            StageError::Changed(_) => "FileChanged",
            StageError::Io(e) => py::os_error_name(e),
        }
    }
}

/// What the question pins of the file (Python compares these fields of its `os.stat_result`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub size: i64,
    pub mtime_ns: i64,
    pub ino: i64,
    pub dev: i64,
}

impl FileStat {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            size: meta.size() as i64,
            mtime_ns: meta.mtime().saturating_mul(1_000_000_000).saturating_add(meta.mtime_nsec()),
            ino: meta.ino() as i64,
            dev: meta.dev() as i64,
        }
    }
}

fn c_name(part: &OsStr) -> io::Result<CString> {
    CString::new(part.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "embedded null byte"))
}

fn open_at(dir: Option<&OwnedFd>, name: &CString, flags: libc::c_int) -> io::Result<OwnedFd> {
    use std::os::fd::AsRawFd;
    let at = dir.map(|d| d.as_raw_fd()).unwrap_or(libc::AT_FDCWD);
    // SAFETY: `name` is a NUL-terminated string that outlives the call; `at` is an open descriptor or AT_FDCWD.
    let fd = unsafe { libc::openat(at, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// py: share.py:297 `def _open_exact(path: Path) -> int:`
///
/// A read-only descriptor on ``path`` itself, walked from the root one component at a time with O_NOFOLLOW:
/// a link anywhere on the way is refused (ELOOP), never followed. ``path`` is the real path the owner-path
/// rules judged, so it holds no link. O_NOFOLLOW on the whole path checks only its last component: a folder
/// swapped for a link to ~/.ssh between the resolve and the open was followed, and the hidden file was asked
/// about under the innocent name and sent (Lean ShareGate P4, 2026-10-05).
pub(crate) fn open_exact(path: &Path) -> Result<std::fs::File, StageError> {
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if !path.is_absolute() {
        return Err(FileChanged::new("not an absolute path").into());
    }
    let parts: Vec<&OsStr> = path
        .components()
        .filter(|c| !matches!(c, Component::RootDir | Component::Prefix(_)))
        .map(|c| c.as_os_str())
        .collect();
    let Some((last, folders)) = parts.split_last() else {
        return Err(FileChanged::new("not a file").into());
    };
    let mut fd = open_at(None, &c_name(OsStr::new("/"))?, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)?;
    for part in folders {
        fd = open_at(Some(&fd), &c_name(part)?, flags | libc::O_DIRECTORY)?;
    }
    let file = open_at(Some(&fd), &c_name(last)?, flags)?;
    Ok(std::fs::File::from(file))
}

/// py: share.py:321 `def fingerprint(path: Path) -> tuple[os.stat_result, str]:`
///
/// (fstat, SHA-256 of the bytes) of the regular file at ``path``, read through one O_NOFOLLOW descriptor.
pub fn fingerprint(path: &Path) -> Result<(FileStat, String), StageError> {
    let mut f = open_exact(path)?;
    let meta = f.metadata()?;
    if !meta.file_type().is_file() {
        return Err(FileChanged::new("not a regular file").into());
    }
    let mut h = Sha256::new();
    let mut block = vec![0u8; CHUNK as usize];
    loop {
        let n = f.read(&mut block)?;
        if n == 0 {
            break;
        }
        h.update(&block[..n]);
    }
    Ok((FileStat::of(&meta), hex::encode(h.finalize())))
}

/// py: share.py:334 `def stage_exact(p: Pending, dest: Path) -> None:`
///
/// Copy the approved bytes to ``dest`` (created new, 0600) through one O_NOFOLLOW descriptor, checking the
/// inode, device, size, mtime and SHA-256 the question saw; on any difference ``dest`` is removed and
/// FileChanged raised. What is sent is this copy, never the path opened again (review, 2026-10-05).
pub fn stage_exact(p: &Pending, dest: &Path) -> Result<(), StageError> {
    let mut src = open_exact(&p.path)?;
    let meta = src.metadata()?;
    let st = FileStat::of(&meta);
    if !meta.file_type().is_file() || (st.ino, st.dev, st.size, st.mtime_ns) != (p.ino, p.dev, p.size, p.mtime_ns) {
        return Err(FileChanged::new("the file changed").into());
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(dest)?;
    let copied = (|| -> Result<(), StageError> {
        let mut h = Sha256::new();
        let mut block = vec![0u8; CHUNK as usize];
        loop {
            let n = src.read(&mut block)?;
            if n == 0 {
                break;
            }
            h.update(&block[..n]);
            out.write_all(&block[..n])?;
        }
        out.flush()?;
        if hex::encode(h.finalize()) != p.sha256 {
            return Err(FileChanged::new("the file's bytes changed").into());
        }
        Ok(())
    })();
    if copied.is_err() {
        drop(out);
        let _ = std::fs::remove_file(dest);
    }
    copied
}


/// py: share.py:359 `class Share:`
///
/// The share tools. ``runner`` runs osascript; ``token`` gives a Google access token (apps_mcp.TokenSource)
/// or None; ``http`` is the Drive client (tests lend a fake transport, Python's httpx.MockTransport).
///
/// Rust: held in an `Arc` by the inlet, so the pending questions and the folder id sit behind locks. Python's
/// module constants `MULTIPART_MAX` and `MAX_GMAIL_ATTACH_BYTES`, which its tests patch, are the fields
/// `multipart_max` and `gmail_attach_max`.
pub struct Share {
    pub config: ShareConfig,
    pub runner: Runner,
    pub token: Option<TokenFn>,
    pub http: Option<Http>,
    pub home: Option<PathBuf>,
    pub outbox: Option<PathBuf>,
    pub clock: Clock,
    /// Gmail may send (gmail_can_send): an email share is an attachment up to its size
    pub gmail: bool,
    /// `MULTIPART_MAX`
    pub multipart_max: usize,
    /// `MAX_GMAIL_ATTACH_BYTES`
    pub gmail_attach_max: usize,
    pub(crate) pending: Mutex<HashMap<String, Pending>>,
    pub(crate) folder_id: Mutex<String>,
}

/// Why a Drive or Gmail step failed: the exceptions `_send_drive_staged` and `_send_gmail` catch.
#[derive(Debug)]
pub(crate) enum StepFail {
    Drive(DriveError),
    /// httpx.HTTPError: `kind` is its class name
    Http(HttpError),
    /// `response.json()` on a body that is not JSON (JSONDecodeError, a ValueError)
    Json,
    /// a key the answer lacks (KeyError)
    Key,
    /// an answer of the wrong shape (AttributeError, TypeError)
    Shape(&'static str),
    Os(io::Error),
}

impl StepFail {
    fn type_name(&self) -> String {
        match self {
            StepFail::Drive(_) => "DriveError".into(),
            StepFail::Http(e) => e.kind.clone(),
            StepFail::Json => "JSONDecodeError".into(),
            StepFail::Key => "KeyError".into(),
            StepFail::Shape(name) => (*name).into(),
            StepFail::Os(e) => py::os_error_name(e).into(),
        }
    }

    /// `str(e) if isinstance(e, DriveError) else type(e).__name__`.
    fn detail(&self) -> String {
        match self {
            StepFail::Drive(e) => e.message.clone(),
            other => other.type_name(),
        }
    }
}

impl From<DriveError> for StepFail {
    fn from(e: DriveError) -> Self {
        StepFail::Drive(e)
    }
}

impl From<io::Error> for StepFail {
    fn from(e: io::Error) -> Self {
        StepFail::Os(e)
    }
}

/// A request's body, as httpx takes it: none, `json=` (compact, as httpx 0.28 writes it) or `content=`.
pub(crate) enum Body {
    None,
    Json(Value),
    Bytes(Vec<u8>),
}

/// `response.json()`.
fn json_of(r: &HttpResponse) -> Result<Value, StepFail> {
    r.json().map_err(|_| StepFail::Json)
}

/// `value["key"]` on a decoded answer.
fn index<'a>(v: &'a Value, key: &str) -> Result<&'a Value, StepFail> {
    match v {
        Value::Object(m) => m.get(key).ok_or(StepFail::Key),
        _ => Err(StepFail::Shape("TypeError")),
    }
}

/// urllib's `quote_plus` (httpx's query encoding): letters, digits and `_.-~` stay, a space is `+`.
fn quote_plus(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The URL with `params` added to its query.
fn with_params(url: &str, params: &[(&str, String)]) -> String {
    if params.is_empty() {
        return url.to_string();
    }
    let query = params.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect::<Vec<_>>().join("&");
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{query}")
}

/// `secrets.token_urlsafe(n)`.
fn token_urlsafe(n: usize) -> String {
    let bytes: Vec<u8> = (0..n).map(|_| rand::random::<u8>()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `secrets.token_hex(n)`.
fn token_hex(n: usize) -> String {
    let bytes: Vec<u8> = (0..n).map(|_| rand::random::<u8>()).collect();
    hex::encode(bytes)
}

/// `str(x or "")` for a JSON value.
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if py::truthy(v) => eve_canvas::py::py_str(v),
        _ => String::new(),
    }
}

/// A JSON object from a `json!` literal.
fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => Obj::new(),
    }
}

/// `{"ok": False, "reason": reason}`.
fn refuse(reason: impl Into<String>) -> Obj {
    obj(json!({"ok": false, "reason": reason.into()}))
}

/// `_CONTROL.sub("", text).strip()`.
fn clean(text: &str) -> String {
    py::strip(&CONTROL.replace_all(text, "")).to_string()
}

/// `path.name`.
fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Python's `str(bool)`.
fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// `json.dumps(value)` with Python's defaults: `", "` and `": "`, every non-ASCII character escaped.
fn dumps_ascii(v: &Value) -> String {
    let mut out = String::new();
    for c in py::dumps(v).chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

const CHANGED: &str = "the file changed since the owner was asked; nothing was sent";

/// The owner's yes/no for one share: the token is cancelled when the wait ends any other way than an answer
/// (Python's `except BaseException: self.cancel(token); raise`).
struct CancelUnlessAnswered<'a> {
    share: &'a Share,
    token: String,
    answered: bool,
}

impl Drop for CancelUnlessAnswered<'_> {
    fn drop(&mut self) {
        if !self.answered {
            self.share.cancel(&self.token);
        }
    }
}

impl Share {
    /// `Share(config)` with every other field at its default (osascript, no Google, `time.monotonic`).
    pub fn new(config: ShareConfig) -> Self {
        Self {
            config,
            runner: osascript(),
            token: None,
            http: None,
            home: None,
            outbox: None,
            clock: Arc::new(monotonic),
            gmail: false,
            multipart_max: MULTIPART_MAX,
            gmail_attach_max: MAX_GMAIL_ATTACH_BYTES,
            pending: Mutex::new(HashMap::new()),
            folder_id: Mutex::new(String::new()),
        }
    }

    /// `runner=`.
    pub fn with_runner(mut self, runner: Runner) -> Self {
        self.runner = runner;
        self
    }

    /// `token=`.
    pub fn with_token(mut self, token: Option<TokenFn>) -> Self {
        self.token = token;
        self
    }

    /// `http=`.
    pub fn with_http(mut self, http: Http) -> Self {
        self.http = Some(http);
        self
    }

    /// `home=`.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// `outbox=`.
    pub fn with_outbox(mut self, outbox: impl Into<PathBuf>) -> Self {
        self.outbox = Some(outbox.into());
        self
    }

    /// `clock=`.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// `gmail=`.
    pub fn with_gmail(mut self, gmail: bool) -> Self {
        self.gmail = gmail;
        self
    }

    /// The question waiting on `token` (Python's `share._pending[token]`).
    pub fn pending(&self, token: &str) -> Option<Pending> {
        self.pending.lock().get(token).cloned()
    }

    /// How many questions wait (`len(share._pending)`).
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    // -- what the model gets --

    /// py: share.py:376 `def email_on(self) -> bool:`
    pub fn email_on(&self) -> bool {
        self.token.is_some()
    }

    /// py: share.py:379 `def tools(self) -> list[dict[str, Any]]:`
    pub fn tools(&self) -> Vec<Obj> {
        let mut tools: Vec<Obj> =
            TOOLS.as_array().map(|a| a.iter().cloned().map(obj).collect()).unwrap_or_default();
        let vias: Vec<Value> = VIAS
            .iter()
            .filter(|v| (**v == "imessage" && self.config.imessage) || (**v == "email" && self.email_on()))
            .map(|v| json!(v))
            .collect();
        if let Some(via) = tools[0]
            .get_mut("parameters")
            .and_then(|p| p.get_mut("properties"))
            .and_then(|p| p.get_mut("via"))
            .and_then(Value::as_object_mut)
        {
            via.insert("enum".into(), Value::Array(vias));
        }
        if self.gmail {
            let description = tools[0].get("description").and_then(Value::as_str).unwrap_or("").to_string();
            tools[0].insert("description".into(), json!(description.replace(EMAIL_AS_LINK, EMAIL_ATTACHED)));
        }
        if !self.config.imessage {
            tools.truncate(1); // find_contact needs Contacts (macOS)
        }
        tools
    }

    /// py: share.py:387 `def instructions(self) -> str:`
    pub fn instructions(&self) -> String {
        let mut text =
            if self.gmail { INSTRUCTIONS.replace(VIA_EMAIL_LINK, VIA_EMAIL_ATTACHED) } else { INSTRUCTIONS.to_string() };
        if !self.config.imessage {
            // no Contacts off the Mac: tools() offers share_file alone, so the block names nothing else (the owner's
            // rule for prompts; Lean RoutedTurn P5 assumes the block and the tools agree)
            text = text.replace(
                "Sharing a file with someone (share_file, find_contact).",
                "Sharing a file with someone (share_file).",
            );
            text = text.replace(" find_contact looks someone up when you only need their details.", "");
        }
        text
    }

    /// py: share.py:396 `async def handle(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:`
    pub async fn handle(&self, name: &str, args: &Obj) -> Obj {
        if !self.config.enabled {
            return refuse(OFF_REASON);
        }
        if name == "find_contact" {
            return self.find_contact(&str_or_empty(args.get("name"))).await;
        }
        if name == "share_file" {
            return self
                .prepare(
                    &str_or_empty(args.get("path")),
                    &str_or_empty(args.get("to")),
                    &str_or_empty(args.get("via")),
                    &str_or_empty(args.get("message")),
                )
                .await;
        }
        refuse(format!("unknown tool {name}"))
    }

    /// py: share.py:406 `async def ask_and_share(self, args: Mapping[str, Any], ask: Ask) -> dict[str, Any]:`
    ///
    /// The whole share_file flow for a caller with a yes/no prompt: the question, the owner's answer, the send.
    /// The token never reaches the model.
    pub async fn ask_and_share(&self, args: &Obj, ask: Ask) -> Obj {
        let prep = self.handle("share_file", args).await;
        if !prep.get("needs_approval").is_some_and(py::truthy) {
            return prep;
        }
        let token = prep.get("token").map(eve_canvas::py::py_str).unwrap_or_default();
        let question = prep.get("question").map(eve_canvas::py::py_str).unwrap_or_default();
        let mut guard = CancelUnlessAnswered { share: self, token: token.clone(), answered: false };
        let reply = ask(question).await;
        guard.answered = true;
        drop(guard);
        let answer = py::strip(&reply).to_lowercase();
        if !eve_consent::approves(&answer) {
            self.cancel(&token);
            return refuse("the owner said no; do not retry it");
        }
        self.confirm(&token).await
    }

    // -- contacts --

    /// py: share.py:426 `async def lookup(self, name: str) -> tuple[Optional[list[dict[str, Any]]], int, str]:`
    ///
    /// (people, how many matched, why not). People carry name, phones and emails as [{label, value}].
    pub async fn lookup(&self, name: &str) -> (Option<Vec<Obj>>, i64, String) {
        if !self.config.imessage {
            return (None, 0, "Contacts can only be read on the Mac".into());
        }
        let q = py::head(&clean(name), 100).to_string();
        if q.is_empty() {
            return (None, 0, "no name given".into());
        }
        let argv = vec!["osascript".into(), "-l".into(), "JavaScript".into(), "-".into(), q, MAX_MATCHES.to_string()];
        let (rc, out, _) = (self.runner)(argv, CONTACTS_JXA.as_bytes().to_vec(), CONTACTS_SECS).await;
        tracing::info!("share: contacts lookup rc={rc}");
        if rc == RC_TIMEOUT {
            return (None, 0, "Contacts didn't answer in time".into());
        }
        if rc != 0 {
            return (
                None,
                0,
                "Contacts can't be read: allow eve under System Settings > Privacy & Security > Contacts (and Automation > Contacts)"
                    .into(),
            );
        }
        let text = py::strip(&out);
        let parsed: Option<Value> = py::loads(if text.is_empty() { "{}" } else { text });
        let Some(Value::Object(body)) = parsed else {
            return (None, 0, "Contacts gave an answer that couldn't be read".into());
        };
        let handles = |p: &Obj, key: &str| -> Vec<Value> {
            let list = match p.get(key) {
                Some(Value::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            list.iter()
                .filter_map(Value::as_object)
                .filter(|x| x.get("value").is_some_and(py::truthy))
                .map(|x| {
                    json!({"label": label(x.get("label").unwrap_or(&Value::Null)),
                           "value": py::strip(&str_or_empty(x.get("value")))})
                })
                .collect()
        };
        let mut people = Vec::new();
        if let Some(Value::Array(found)) = body.get("people") {
            for p in found.iter().filter_map(Value::as_object) {
                people.push(obj(json!({
                    "name": py::strip(&str_or_empty(p.get("name"))),
                    "phones": handles(p, "phones"),
                    "emails": handles(p, "emails"),
                })));
            }
        }
        tracing::info!("share: contacts matched {}", people.len());
        let count = match body.get("count") {
            Some(v) if py::truthy(v) => match v {
                Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
                Value::String(s) => s.py_trim().parse::<i64>().ok(),
                Value::Bool(true) => Some(1),
                _ => None,
            }
            .unwrap_or(people.len() as i64),
            _ => people.len() as i64,
        };
        (Some(people), count, String::new())
    }

    /// py: share.py:455 `async def find_contact(self, name: str) -> dict[str, Any]:`
    pub async fn find_contact(&self, name: &str) -> Obj {
        let (people, count, why) = self.lookup(name).await;
        let Some(people) = people else {
            return refuse(why);
        };
        let n = people.len() as i64;
        let mut out = obj(json!({"ok": true, "people": people}));
        if count > n {
            out.insert("more".into(), json!(format!("{} more matched; use more of the name", count - n)));
        }
        out
    }

    /// py: share.py:464 `async def _recipient(self, to: str, via: str) -> dict[str, Any]:`
    ///
    /// {"handle", "who"} or {"ok": False, ...} (with ``matches`` when the owner must pick).
    pub(crate) async fn recipient(&self, to: &str, via: &str) -> Obj {
        let to = clean(to);
        if to.is_empty() {
            return refuse("no recipient given");
        }
        if is_email(&to) {
            return obj(json!({"handle": to, "who": to}));
        }
        if is_phone(&to) {
            if via == "email" {
                return refuse("email needs an email address, not a phone number");
            }
            return obj(json!({"handle": phone_handle(&to), "who": to}));
        }
        let (people, count, why) = self.lookup(&to).await;
        let Some(people) = people else {
            return refuse(why);
        };
        if people.is_empty() {
            return refuse("no one by that name in the owner's Contacts");
        }
        if count > 1 {
            return obj(json!({"ok": false, "ambiguous": true, "matches": people,
                              "reason": "more than one person matches: ask the owner which, then pass their number or address as `to`"}));
        }
        let person = &people[0];
        let pname = person.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let values = |key: &str, only: Option<&[&str]>| -> Vec<String> {
            person
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|x| {
                            only.is_none_or(|labels| {
                                labels.contains(&x.get("label").and_then(Value::as_str).unwrap_or(""))
                            })
                        })
                        .map(|x| x.get("value").and_then(Value::as_str).unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut options = if via == "email" {
            values("emails", None)
        } else {
            let mobiles = values("phones", Some(&["mobile", "iphone", "cell"]));
            if mobiles.len() == 1 {
                mobiles
            } else {
                let mut all = values("phones", None);
                all.extend(values("emails", None));
                all
            }
        };
        let mut seen = HashSet::new();
        options.retain(|o| seen.insert(o.clone())); // dict.fromkeys: first of each, in order
        if options.is_empty() {
            let kind = if via == "email" { "email address" } else { "phone number or email address" };
            return refuse(format!("{pname} has no {kind} in Contacts"));
        }
        if options.len() > 1 {
            return obj(json!({"ok": false, "ambiguous": true, "matches": [person],
                              "reason": format!("{pname} has several; ask the owner which, then pass it as `to`")}));
        }
        let mut handle = options[0].clone();
        if via == "imessage" && is_phone(&handle) {
            handle = phone_handle(&handle);
        }
        obj(json!({"handle": handle, "who": format!("{pname} ({})", options[0])}))
    }

    // -- the question --

    /// py: share.py:504 `async def prepare(self, raw_path: str, to: str, via: str, note: str) -> dict[str, Any]:`
    pub async fn prepare(&self, raw_path: &str, to: &str, via: &str, note: &str) -> Obj {
        if !VIAS.contains(&via) {
            return refuse("via is imessage or email");
        }
        if via == "imessage" && !self.config.imessage {
            return refuse("iMessage is only on the Mac");
        }
        if via == "email" && !self.email_on() {
            return refuse(
                "email sharing needs the Google sign-in (eve-bridge apps login google) with Drive allowed to write",
            );
        }
        let (path, why) = resolve_file(raw_path, self.home.as_deref());
        let Some(path) = path else {
            return refuse(why);
        };
        // What the owner approves is these exact bytes at this inode (review, 2026-10-05: size and mtime alone,
        // read through a link, let a swapped-in link to a hidden key go out); stage_exact checks them at the send.
        let read = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fingerprint(&path)).await
        };
        let Ok(Ok((st, digest))) = read else {
            return refuse("that file can't be read");
        };
        let cap = if via == "imessage" { MAX_IMESSAGE_BYTES } else { MAX_DRIVE_BYTES } as i64;
        let attach = via == "email" && self.gmail && st.size <= self.gmail_attach_max as i64;
        if st.size > cap {
            return refuse(format!(
                "too big to share ({}; the limit is {})",
                human_size(st.size),
                human_size(cap)
            ));
        }
        let rec = self.recipient(to, via).await;
        if !rec.contains_key("handle") {
            return rec;
        }
        let handle = rec.get("handle").and_then(Value::as_str).unwrap_or("").to_string();
        let who = rec.get("who").and_then(Value::as_str).unwrap_or("").to_string();
        let note = py::head(&clean(note), MAX_NOTE).to_string();
        self.sweep_pending();
        let token = token_urlsafe(12);
        let pending = Pending {
            token: token.clone(),
            via: via.to_string(),
            path: path.clone(),
            size: st.size,
            mtime_ns: st.mtime_ns,
            handle,
            who: who.clone(),
            note: note.clone(),
            created: (self.clock)(),
            raw: raw_path.to_string(),
            attach,
            ino: st.ino,
            dev: st.dev,
            sha256: digest,
        };
        self.pending.lock().insert(token.clone(), pending);
        let how = if via == "imessage" {
            "by iMessage"
        } else if attach {
            "by email, attached, from your Gmail"
        } else {
            "by email (a Google Drive link)"
        };
        let mut question = format!("Send {} ({}) to {who} {how}?", file_name(&path), human_size(st.size));
        if !note.is_empty() {
            question.push_str(&format!("\nNote: \"{note}\""));
        }
        tracing::info!("share: asking ({via}, {} bytes)", st.size);
        obj(json!({"ok": true, "needs_approval": true, "question": question, "token": token}))
    }

    /// py: share.py:543 `def _sweep_pending(self) -> None:`
    pub(crate) fn sweep_pending(&self) {
        let now = (self.clock)();
        self.pending.lock().retain(|_, p| now - p.created <= APPROVAL_TTL_SECS);
    }

    /// py: share.py:548 `def cancel(self, token: str) -> None:`
    pub fn cancel(&self, token: &str) {
        self.pending.lock().remove(token);
    }

    // -- the send --

    /// py: share.py:552 `async def confirm(self, token: str) -> dict[str, Any]:`
    ///
    /// Send what the question described, once. Nothing runs for an unknown, used or expired token, or when
    /// the file changed since the question.
    pub async fn confirm(&self, token: &str) -> Obj {
        let taken = self.pending.lock().remove(token);
        let p = match taken {
            Some(p) if (self.clock)() - p.created <= APPROVAL_TTL_SECS => p,
            _ => {
                tracing::info!("share: refused a token that is unknown or expired");
                return refuse("that share is no longer pending; ask again");
            }
        };
        let (real, _why) = resolve_file(&p.raw, self.home.as_deref()); // the owner-path rules again, at the send
        if real.as_deref() != Some(p.path.as_path()) {
            tracing::info!("share: refused, the path no longer leads to the file the owner was asked about");
            return refuse(CHANGED);
        }
        if p.via == "imessage" {
            return self.send_imessage(&p).await;
        }
        if p.attach {
            return self.send_gmail(&p).await;
        }
        self.send_drive(&p).await
    }

    /// py: share.py:569 `async def _send_imessage(self, p: Pending) -> dict[str, Any]:`
    pub(crate) async fn send_imessage(&self, p: &Pending) -> Obj {
        let outbox = match &self.outbox {
            Some(dir) => py::expand_user(dir),
            None => expanduser(OUTBOX),
        };
        let staged = outbox.join(format!("{}-{}", token_hex(4), file_name(&p.path)));
        let prepared = std::fs::create_dir_all(&outbox).and_then(|()| self.sweep_outbox(&outbox));
        let staging = match prepared {
            Err(e) => Err(StageError::Io(e)),
            Ok(()) => {
                let (staged, p) = (staged.clone(), p.clone());
                tokio::task::spawn_blocking(move || stage_exact(&p, &staged))
                    .await
                    .unwrap_or_else(|e| Err(StageError::Io(io::Error::other(e.to_string()))))
            }
        };
        match staging {
            Ok(()) => {}
            Err(StageError::Changed(_)) => {
                tracing::info!("share: refused, the file changed since the question");
                return refuse(CHANGED);
            }
            Err(e) => {
                tracing::warn!("share: could not stage the file ({})", e.type_name());
                return refuse("couldn't copy the file for Messages; nothing was sent");
            }
        }
        let argv = vec!["osascript".into(), "-".into(), p.handle.clone(), staged.to_string_lossy().into_owned(), p.note.clone()];
        let (rc, _, err) = (self.runner)(argv, MESSAGES_SEND.as_bytes().to_vec(), SEND_SECS).await;
        tracing::info!("share: imessage rc={rc} ({} bytes)", p.size);
        if rc == RC_TIMEOUT {
            return refuse(
                "Messages didn't answer in time, so it may or may not have sent; check Messages before trying again",
            );
        }
        if rc != 0 {
            let low = err.to_lowercase();
            let why = if low.contains("-1743") || low.contains("not authorized") {
                "eve isn't allowed to control Messages: System Settings > Privacy & Security > Automation, allow Messages for eve"
            } else {
                "Messages refused it (is iMessage signed in, and is that number or address on iMessage?)"
            };
            let _ = std::fs::remove_file(&staged);
            return refuse(why);
        }
        obj(json!({"ok": true, "sent": file_name(&p.path), "to": p.who, "via": "imessage", "bytes": p.size,
                   "note": "Messages took it; delivery isn't confirmed by Messages"}))
    }

    /// py: share.py:600 `def _sweep_outbox(self, outbox: Path) -> None:`
    ///
    /// Copies older than OUTBOX_KEEP_SECS go (Messages has copied the file into its own attachments by then).
    pub(crate) fn sweep_outbox(&self, outbox: &Path) -> io::Result<()> {
        let cutoff = wall() - OUTBOX_KEEP_SECS;
        for entry in std::fs::read_dir(outbox)? {
            let Ok(entry) = entry else {
                continue;
            };
            let f = entry.path();
            if let Ok(meta) = std::fs::metadata(&f) {
                let mtime = meta.mtime() as f64 + meta.mtime_nsec() as f64 / 1e9;
                if meta.is_file() && mtime < cutoff {
                    let _ = std::fs::remove_file(&f);
                }
            }
        }
        Ok(())
    }

    // -- Gmail --

    /// py: share.py:610 `def _send_gmail(self, p: Pending) -> dict[str, Any]:`
    ///
    /// The approved bytes, staged, as one email's attachment from the owner's Gmail.
    pub(crate) async fn send_gmail(&self, p: &Pending) -> Obj {
        let built = {
            let p = p.clone();
            tokio::task::spawn_blocking(move || -> Result<Result<Vec<u8>, MailError>, StageError> {
                let tmp = tempfile::Builder::new().prefix("eve-share-").tempdir()?;
                let staged = tmp.path().join(file_name(&p.path));
                stage_exact(&p, &staged)?;
                let data = std::fs::read(&staged)?;
                Ok(attachment_mail(&p.handle, &file_name(&p.path), &p.note, &data))
            })
            .await
            .unwrap_or_else(|e| Err(StageError::Io(io::Error::other(e.to_string()))))
        };
        let mail = match built {
            Ok(Ok(mail)) => mail,
            Err(StageError::Changed(_)) => {
                tracing::info!("share: refused, the file changed since the question");
                return refuse(CHANGED);
            }
            Err(e) => {
                tracing::warn!("share: could not stage the file ({})", e.type_name());
                return refuse("couldn't read the file; nothing was sent");
            }
            Ok(Err(_)) => {
                // a name no email header can carry (a line break)
                tracing::info!("share: refused, the file's name cannot be emailed");
                return refuse("that file's name can't be emailed; rename it and ask again; nothing was sent");
            }
        };
        let sent = match self
            .drive(
                "POST",
                GMAIL_SEND_UPLOAD,
                &[("uploadType", "media".into())],
                Body::Bytes(mail),
                &[("Content-Type", "message/rfc822".into())],
            )
            .await
            .and_then(|r| json_of(&r))
        {
            Ok(sent) => sent,
            Err(e) => {
                tracing::warn!("share: gmail send failed ({})", e.type_name());
                let detail = match &e {
                    StepFail::Drive(d) => d.message.replace("Drive said", "Gmail said"),
                    other => other.type_name(),
                };
                return refuse(format!("{detail}; nothing was sent"));
            }
        };
        tracing::info!("share: gmail sent ({} bytes)", p.size);
        let id = match &sent {
            Value::Object(m) => m.get("id").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        obj(json!({"ok": true, "sent": file_name(&p.path), "to": p.who, "via": "email", "bytes": p.size,
                   "attached": true, "id": id, "note": "sent from the owner's Gmail"}))
    }

    // -- Drive --

    /// py: share.py:639 `def _drive(self, method: str, url: str, **kw: Any) -> httpx.Response:`
    ///
    /// One authorized request (`params`, a body, extra headers); an answer of 400 or more is a DriveError naming
    /// Google's message.
    pub(crate) async fn drive(
        &self,
        method: &str,
        url: &str,
        params: &[(&str, String)],
        body: Body,
        headers: &[(&str, String)],
    ) -> Result<HttpResponse, StepFail> {
        let tok = match &self.token {
            Some(token) => token().await,
            None => None,
        };
        let Some(tok) = tok.filter(|t| !t.is_empty()) else {
            return Err(DriveError { message: "not signed in to Google (eve-bridge apps login google)".into() }.into());
        };
        let mut request = HttpRequest {
            method: method.to_string(),
            url: with_params(url, params),
            headers: vec![("Authorization".into(), format!("Bearer {tok}"))],
            body: Vec::new(),
            timeout: None,
        };
        match body {
            Body::None => {}
            Body::Json(v) => {
                request.headers.push(("Content-Type".into(), "application/json".into()));
                request.body = serde_json::to_vec(&v).unwrap_or_default();
            }
            Body::Bytes(b) => request.body = b,
        }
        for (k, v) in headers {
            request.headers.retain(|(name, _)| !name.eq_ignore_ascii_case(k));
            request.headers.push((k.to_string(), v.clone()));
        }
        let client = match &self.http {
            Some(http) => http.clone(),
            None => {
                request.timeout = Some(DRIVE_SECS);
                real_http()
            }
        };
        let r = client.send(request).await.map_err(StepFail::Http)?;
        if r.status >= 400 {
            let msg = match r.json() {
                Ok(Value::Object(m)) => match m.get("error") {
                    None => None,
                    Some(Value::Object(e)) => e.get("message").filter(|v| py::truthy(v)).map(eve_canvas::py::py_str),
                    Some(_) => None, // AttributeError: the text
                },
                _ => None,
            }
            .unwrap_or_else(|| r.text());
            return Err(DriveError { message: format!("Drive said HTTP {}: {}", r.status, py::head(&msg, 200)) }.into());
        }
        Ok(r)
    }

    /// py: share.py:658 `def _folder(self) -> str:`
    ///
    /// The "eve shared" folder eve made (found by its appProperties), made when there is none.
    pub(crate) async fn folder(&self) -> Result<String, StepFail> {
        let known = self.folder_id.lock().clone();
        if !known.is_empty() {
            return Ok(known);
        }
        let (key, value) = (FOLDER_PROP[0], FOLDER_PROP[1]);
        let q = format!("mimeType='{FOLDER_MIME}' and trashed=false and appProperties has {{ key='{key}' and value='{value}' }}");
        let found = json_of(
            &self
                .drive(
                    "GET",
                    &format!("{DRIVE}/files"),
                    &[("q", q), ("fields", "files(id)".into()), ("pageSize", "1".into())],
                    Body::None,
                    &[],
                )
                .await?,
        )?;
        let files = match &found {
            Value::Object(m) => m.get("files").cloned().unwrap_or(Value::Null),
            _ => return Err(StepFail::Shape("AttributeError")),
        };
        let id = match &files {
            Value::Array(items) if !items.is_empty() => eve_canvas::py::py_str(index(&items[0], "id")?),
            v if py::truthy(v) => return Err(StepFail::Key),
            _ => {
                let made = self
                    .drive(
                        "POST",
                        &format!("{DRIVE}/files"),
                        &[("fields", "id".into())],
                        Body::Json(json!({"name": FOLDER_NAME, "mimeType": FOLDER_MIME, "appProperties": {key: value}})),
                        &[],
                    )
                    .await?;
                eve_canvas::py::py_str(index(&json_of(&made)?, "id")?)
            }
        };
        *self.folder_id.lock() = id.clone();
        Ok(id)
    }

    /// py: share.py:674 `def _upload(self, path: Path, size: int, folder: str) -> dict[str, Any]:`
    ///
    /// Drive v3 files.create: multipart up to `multipart_max`, resumable above.
    pub(crate) async fn upload(&self, path: &Path, size: i64, folder: &str) -> Result<Value, StepFail> {
        let name = file_name(path);
        let mime = eve_link_uploads::mimetypes::guess_type(&name).0.unwrap_or_else(|| "application/octet-stream".into());
        let meta = json!({"name": name, "parents": [folder]});
        if size <= self.multipart_max as i64 {
            let boundary = format!("eve-share-{}", token_hex(8));
            let mut body = format!(
                "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: {mime}\r\n\r\n",
                dumps_ascii(&meta)
            )
            .into_bytes();
            body.extend(std::fs::read(path)?);
            body.extend(format!("\r\n--{boundary}--").into_bytes());
            let r = self
                .drive(
                    "POST",
                    DRIVE_UPLOAD,
                    &[("uploadType", "multipart".into()), ("fields", FILE_FIELDS.into())],
                    Body::Bytes(body),
                    &[("Content-Type", format!("multipart/related; boundary={boundary}"))],
                )
                .await?;
            return json_of(&r);
        }
        let start = self
            .drive(
                "POST",
                DRIVE_UPLOAD,
                &[("uploadType", "resumable".into()), ("fields", FILE_FIELDS.into())],
                Body::Json(meta),
                &[("X-Upload-Content-Type", mime.clone()), ("X-Upload-Content-Length", size.to_string())],
            )
            .await?;
        let session = start.header("location").unwrap_or("").to_string();
        if session.is_empty() {
            return Err(DriveError { message: "Drive gave no upload address".into() }.into());
        }
        let content: Vec<u8> = chunks(path)?.concat();
        let r = self
            .drive(
                "PUT",
                &session,
                &[],
                Body::Bytes(content),
                &[("Content-Type", mime), ("Content-Length", size.to_string())],
            )
            .await?;
        json_of(&r)
    }

    /// py: share.py:692 `def _send_drive(self, p: Pending) -> dict[str, Any]:`
    pub(crate) async fn send_drive(&self, p: &Pending) -> Obj {
        // The approved bytes are copied first, to a private folder, and the upload reads that copy (stage_exact).
        let staged = {
            let p = p.clone();
            tokio::task::spawn_blocking(move || -> Result<(tempfile::TempDir, PathBuf), StageError> {
                let tmp = tempfile::Builder::new().prefix("eve-share-").tempdir()?;
                let staged = tmp.path().join(file_name(&p.path));
                stage_exact(&p, &staged)?;
                Ok((tmp, staged))
            })
            .await
            .unwrap_or_else(|e| Err(StageError::Io(io::Error::other(e.to_string()))))
        };
        match staged {
            Ok((tmp, staged)) => {
                let out = self.send_drive_staged(p, &staged).await;
                drop(tmp);
                out
            }
            Err(StageError::Changed(_)) => {
                tracing::info!("share: refused, the file changed since the question");
                refuse(CHANGED)
            }
            Err(e) => {
                tracing::warn!("share: could not stage the file ({})", e.type_name());
                refuse("couldn't read the file; nothing was sent")
            }
        }
    }

    /// py: share.py:706 `def _send_drive_staged(self, p: Pending, staged: Path) -> dict[str, Any]:`
    pub(crate) async fn send_drive_staged(&self, p: &Pending, staged: &Path) -> Obj {
        let mut step = "folder";
        let shared = async {
            let folder = self.folder().await?;
            step = "upload";
            let meta = self.upload(staged, p.size, &folder).await?;
            step = "share";
            let id = eve_canvas::py::py_str(index(&meta, "id")?);
            let mut params = vec![("sendNotificationEmail", "true".to_string()), ("fields", "id".to_string())];
            if !p.note.is_empty() {
                params.push(("emailMessage", p.note.clone()));
            }
            self.drive(
                "POST",
                &format!("{DRIVE}/files/{id}/permissions"),
                &params,
                Body::Json(json!({"type": "user", "role": "reader", "emailAddress": p.handle})),
                &[],
            )
            .await?;
            Ok::<Value, StepFail>(meta)
        }
        .await;
        let meta = match shared {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!("share: drive {step} failed ({})", e.type_name());
                let what = match step {
                    "share" => "the file is in the owner's Drive (eve shared) but was NOT shared",
                    _ => "nothing was uploaded",
                };
                return refuse(format!("{}; {what}", e.detail()));
            }
        };
        tracing::info!("share: drive shared ({} bytes)", p.size);
        let link = str_or_empty(meta.get("webViewLink"));
        obj(json!({"ok": true, "sent": file_name(&p.path), "to": p.who, "via": "email", "bytes": p.size,
                   "link": link, "note": "Google emails them the link"}))
    }
}

/// py: share.py:729 `class DriveError(Exception):`
/// (Python bases: Exception.)
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct DriveError {
    /// `str(error)`.
    pub message: String,
}

/// A value no email header can carry: Python's `ValueError("Header values may not contain linefeed or carriage
/// return characters")`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Header values may not contain linefeed or carriage return characters")]
pub struct MailError;

/// Python's `email.policy.SMTP.max_line_length`.
const MAIL_LINE: usize = 78;

/// `str.splitlines()`'s boundaries.
fn breaks_line(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `bytes.splitlines()`: lines split at `\r\n`, `\r` or `\n`, the breaks dropped.
fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < data.len() {
        match data[i] {
            b'\n' => {
                lines.push(&data[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                lines.push(&data[start..i]);
                i += if data.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

/// `binascii.b2a_base64(data)`: one line, newline-terminated.
fn b2a_base64(data: &[u8]) -> String {
    format!("{}\n", base64::engine::general_purpose::STANDARD.encode(data))
}

/// email's `_encode_base64(data, max_line_length)`: lines of `max // 4 * 3` bytes, each newline-terminated.
fn encode_base64_lines(data: &[u8], max_line_length: usize) -> String {
    let per_line = max_line_length / 4 * 3;
    data.chunks(per_line).map(b2a_base64).collect()
}

/// A byte quoted-printable leaves as it is in a body: printable ASCII but `=`, and the tab.
fn qp_body_safe(b: u8) -> bool {
    (b' '..=b'~').contains(&b) && b != b'=' || b == b'\t'
}

/// `quoprimime.body_encode(body, maxlinelen)` with eol "\n", for a body read as latin-1 (one char per byte).
fn qp_body_encode(body: &[u8], maxlinelen: usize) -> String {
    if body.is_empty() {
        return String::new();
    }
    // quote special characters (line breaks stay)
    let mut text = String::new();
    for &b in body {
        if b == b'\r' || b == b'\n' || qp_body_safe(b) {
            text.push(b as char);
        } else {
            text.push_str(&format!("={b:02X}"));
        }
    }
    let soft_break = "=\n";
    // leave space for the '=' at the end of a line
    let maxlinelen1 = maxlinelen - 1;
    let mut encoded: Vec<String> = Vec::new();
    for line in byte_lines(text.as_bytes()) {
        // every byte is ASCII now: indexes are characters
        let line = std::str::from_utf8(line).unwrap_or("");
        let b = line.as_bytes();
        let len = b.len() as isize;
        // break up the line into pieces no longer than maxlinelen - 1
        let mut start: isize = 0;
        let laststart: isize = len - 1 - maxlinelen as isize;
        while start <= laststart {
            let stop = start + maxlinelen1 as isize;
            // make sure we don't break up an escape sequence
            if b[(stop - 2) as usize] == b'=' {
                encoded.push(line[start as usize..(stop - 1) as usize].to_string());
                start = stop - 2;
            } else if b[(stop - 1) as usize] == b'=' {
                encoded.push(line[start as usize..stop as usize].to_string());
                start = stop - 1;
            } else {
                encoded.push(format!("{}=", &line[start as usize..stop as usize]));
                start = stop;
            }
        }
        // handle rest of line, special case if line ends in whitespace
        let last = b.last().copied();
        if matches!(last, Some(b' ') | Some(b'\t')) {
            let ws = last.unwrap_or(b' ');
            let room = start - laststart;
            let q = if room >= 3 {
                // room for the three-character quoted encoding
                format!("={ws:02X}")
            } else if room == 2 {
                // room for the whitespace character and a soft break
                format!("{}{soft_break}", ws as char)
            } else {
                // room only for a soft break: the quoted whitespace is the only content on the next line
                format!("{soft_break}={ws:02X}")
            };
            let end = (len - 1).max(0) as usize;
            let begin = (start.max(0) as usize).min(end);
            encoded.push(format!("{}{q}", &line[begin..end]));
        } else {
            encoded.push(line[(start.max(0) as usize).min(b.len())..].to_string());
        }
    }
    // add back final newline if present
    if matches!(body.last(), Some(b'\r') | Some(b'\n')) {
        encoded.push(String::new());
    }
    encoded.join("\n")
}

/// email.contentmanager's `_encode_text(note, "utf-8", None, SMTP)`: (Content-Transfer-Encoding, payload).
fn encode_text(text: &str) -> (&'static str, String) {
    let raw = text.as_bytes();
    let lines = byte_lines(raw);
    let normal_body = |lines: &[&[u8]]| -> Vec<u8> {
        let mut out = lines.join(&b'\n');
        out.push(b'\n');
        out
    };
    let embedded_body = |lines: &[&[u8]]| -> Vec<u8> {
        let mut out = lines.join(&b"\r\n"[..]);
        out.extend_from_slice(b"\r\n");
        out
    };
    if lines.iter().map(|l| l.len()).max().unwrap_or(0) <= MAIL_LINE {
        let body = normal_body(&lines);
        if body.is_ascii() {
            return ("7bit", String::from_utf8_lossy(&body).into_owned());
        }
        // the SMTP policy's cte_type is 8bit
        return ("8bit", String::from_utf8_lossy(&body).into_owned());
    }
    let sniff = embedded_body(&lines[..lines.len().min(10)]);
    let sniff_qp = qp_body_encode(&sniff, MAIL_LINE);
    let sniff_base64 = b2a_base64(&sniff);
    // This is a little unfair to qp; it includes lineseps, base64 doesn't.
    if sniff_qp.len() > sniff_base64.len() {
        return ("base64", encode_base64_lines(&embedded_body(&lines), MAIL_LINE));
    }
    if lines.len() <= 10 {
        return ("quoted-printable", sniff_qp);
    }
    ("quoted-printable", qp_body_encode(&normal_body(&lines), MAIL_LINE))
}

/// Every line break of a payload as CRLF (the generator writes with the policy's linesep).
fn crlf(payload: &str) -> String {
    let mut out = String::with_capacity(payload.len() + payload.len() / 32);
    let mut chars = payload.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            c => out.push(c),
        }
    }
    out
}

/// One RFC 2047 encoded word: "q" or "b", as email's `_ew.encode` chooses (q unless it is 5 or more longer).
fn header_q(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'!' | b'*' | b'+' | b'/' => out.push(b as char),
            b' ' => out.push('_'),
            _ => out.push_str(&format!("={b:02X}")),
        }
    }
    out
}

fn prefer_q(text: &str) -> bool {
    let q = header_q(text.as_bytes()).len();
    let b = text.len().div_ceil(3) * 4;
    q < b + 5
}

fn encoded_word(text: &str, q: bool) -> String {
    if q {
        format!("=?utf-8?q?{}?=", header_q(text.as_bytes()))
    } else {
        format!("=?utf-8?b?{}?=", base64::engine::general_purpose::STANDARD.encode(text.as_bytes()))
    }
}

/// `_ew.encode(text)`: q unless it is five or more characters longer than b.
fn ew_encode(text: &str) -> String {
    encoded_word(text, prefer_q(text))
}

fn is_wsp(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// `_steal_trailing_WSP_if_exists(lines)`.
fn steal_trailing_wsp(lines: &mut Vec<String>) -> String {
    let Some(last) = lines.last_mut() else {
        return String::new();
    };
    match last.chars().last() {
        Some(c) if is_wsp(c) => {
            last.pop();
            if last.is_empty() {
                // gh-142006: a line left empty goes
                lines.pop();
            }
            c.to_string()
        }
        _ => String::new(),
    }
}

/// `len(s)`: Python counts characters.
fn clen(s: &str) -> usize {
    s.chars().count()
}

/// `len(lines[-1])`.
fn last_len(lines: &[String]) -> usize {
    lines.last().map(|l| clen(l)).unwrap_or(0)
}

/// `lines[-1]`, made when `lines` is empty (Python would raise; the folds below never leave it empty there).
fn last_line(lines: &mut Vec<String>) -> &mut String {
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines.last_mut().expect("a line")
}

/// The encoded word last laid on the last line: where it starts and ends there (byte offsets), and the text it
/// encodes (what Python gets back by parsing the line from `last_ew` again with `get_unstructured`).
#[derive(Clone)]
struct LastEw {
    start: usize,
    end: usize,
    text: String,
}

/// email's `_fold_as_ew(to_encode, lines, maxlen, last_ew, ew_combine_allowed, charset, last_word_is_ew)`; every
/// word here is encoded as utf-8 (`encode_as` is utf-8 for a us-ascii part too).
fn fold_as_ew(
    mut to_encode: String,
    lines: &mut Vec<String>,
    last_ew: Option<LastEw>,
    ew_combine_allowed: bool,
    last_word_is_ew: bool,
) -> Option<LastEw> {
    let combined = last_ew.filter(|_| ew_combine_allowed);
    if let Some(ew) = &combined {
        // decode the encoded word already on the line, combine it with what follows it and with to_encode
        let line = last_line(lines);
        to_encode = format!("{}{}{to_encode}", ew.text, &line[ew.end..]);
        line.truncate(ew.start);
    } else if last_word_is_ew {
        // following an encoded word with another: whitespace between them is dropped when decoded, so it is
        // encoded in the second
        let line = last_line(lines);
        let kept = line.trim_end_matches(is_wsp).len();
        let leading = line[kept..].to_string();
        line.truncate(kept);
        if !leading.is_empty() {
            line.push(' ');
        }
        to_encode = format!("{leading}{to_encode}");
    } else if to_encode.starts_with(is_wsp) {
        // joining this to non-encoded text: the leading blank stays plain
        let lead = to_encode.remove(0);
        if last_len(lines) == MAIL_LINE {
            let stolen = steal_trailing_wsp(lines);
            lines.push(stolen);
        }
        last_line(lines).push(lead);
    }
    let mut trailing = String::new();
    if to_encode.ends_with(is_wsp) {
        trailing = to_encode.pop().map(String::from).unwrap_or_default();
    }
    let mut new_last = match &combined {
        Some(ew) => ew.start,
        None => last_line(lines).len(),
    };
    // "=?utf-8?" + "?" + "?=" around the text, and the encoding's letter
    let chrome_len = "utf-8".len() + 7;
    let mut last_text = String::new();
    let mut last_end = new_last;
    let mut rest: Vec<char> = to_encode.chars().collect();
    while !rest.is_empty() {
        let remaining = MAIL_LINE as isize - last_len(lines) as isize;
        let text_space = remaining - chrome_len as isize;
        if text_space <= 0 {
            let stolen = steal_trailing_wsp(lines);
            lines.push(if stolen.is_empty() { " ".into() } else { stolen });
            new_last = last_line(lines).len();
            continue;
        }
        let mut take = (text_space as usize).min(rest.len());
        let mut word: String = rest[..take].iter().collect();
        let mut encoded = ew_encode(&word);
        while encoded.len() as isize > remaining && take > 0 {
            take -= 1;
            word = rest[..take].iter().collect();
            encoded = ew_encode(&word);
        }
        let line = last_line(lines);
        line.push_str(&encoded);
        last_end = line.len();
        last_text = word;
        rest.drain(..take);
        if !rest.is_empty() {
            lines.push(" ".into());
            new_last = last_line(lines).len();
        }
    }
    last_line(lines).push_str(&trailing);
    if !ew_combine_allowed {
        return None;
    }
    Some(LastEw { start: new_last, end: last_end, text: last_text })
}

/// A node of a header's parse tree, as email's `_header_value_parser` builds it and `_refold_parse_tree` walks it.
#[derive(Clone)]
enum Tok {
    /// a TokenList: `ew` is its class's fixed `as_ew_allowed` (None: all of its parts allow it)
    List { kids: Vec<Tok>, ew: Option<bool> },
    /// a Terminal: its text, its token type, and whether it is a WhiteSpaceTerminal
    Term { text: String, ttype: &'static str, ws: bool },
    /// `end_ew_not_allowed`: where a part that may not be encoded ends
    EndBlocked,
}

impl Tok {
    fn list(kids: Vec<Tok>) -> Self {
        Tok::List { kids, ew: None }
    }

    fn fixed(kids: Vec<Tok>, ew: bool) -> Self {
        Tok::List { kids, ew: Some(ew) }
    }

    fn value(text: &str, ttype: &'static str) -> Self {
        Tok::Term { text: text.to_string(), ttype, ws: false }
    }

    fn space(text: &str, ttype: &'static str) -> Self {
        Tok::Term { text: text.to_string(), ttype, ws: true }
    }

    /// `str(part)`.
    fn text(&self) -> String {
        match self {
            Tok::List { kids, .. } => kids.iter().map(Tok::text).collect(),
            Tok::Term { text, .. } => text.clone(),
            Tok::EndBlocked => String::new(),
        }
    }

    fn as_ew_allowed(&self) -> bool {
        match self {
            Tok::List { ew: Some(ew), .. } => *ew,
            Tok::List { kids, ew: None } => kids.iter().all(Tok::as_ew_allowed),
            _ => true,
        }
    }

    fn startswith_fws(&self) -> bool {
        match self {
            Tok::List { kids, .. } => kids.first().is_some_and(Tok::startswith_fws),
            Tok::Term { text, ws, .. } => *ws && text.starts_with(is_wsp),
            Tok::EndBlocked => false,
        }
    }

    fn token_type(&self) -> &'static str {
        match self {
            Tok::Term { ttype, .. } => ttype,
            _ => "",
        }
    }
}

/// email's `_refold_parse_tree(parse_tree, policy=SMTP)` (max_line_length 78, us-ascii): the lines, joined with
/// CRLF and ended with one. Every token list here breaks syntactically and lets its encoded words combine, and no
/// part carries undecodable bytes; mime parameters are folded by [`disposition_header`].
fn refold(parse_tree: &[Tok]) -> String {
    let maxlen = MAIL_LINE;
    let mut lines: Vec<String> = vec![String::new()];
    let mut last_word_is_ew = false;
    let mut last_ew: Option<LastEw> = None;
    let mut wrap_as_ew_blocked = 0usize;
    let mut want_encoding = false;
    let mut parts: VecDeque<Tok> = parse_tree.iter().cloned().collect();
    while let Some(part) = parts.pop_front() {
        if matches!(part, Tok::EndBlocked) {
            wrap_as_ew_blocked = wrap_as_ew_blocked.saturating_sub(1);
            continue;
        }
        let tstr = part.text();
        if !want_encoding {
            want_encoding = if matches!(part.token_type(), "ptext" | "vtext") {
                // encode if tstr contains special characters
                tstr.chars().any(|c| "()<>@,:;.\\\"[]\r\n".contains(c))
            } else {
                // encode if tstr contains newlines
                tstr.contains(['\r', '\n'])
            };
        }
        if !tstr.is_ascii() {
            want_encoding = true;
        }
        if want_encoding && wrap_as_ew_blocked == 0 {
            if !part.as_ew_allowed() {
                want_encoding = false;
                last_ew = None;
                if let Tok::List { kids, .. } = &part {
                    let folded = refold(kids);
                    let encoded = folded.strip_suffix("\r\n").unwrap_or(&folded);
                    if !encoded.contains("\r\n") {
                        // it fits on a single line
                        if clen(encoded) as isize > maxlen as isize - last_len(&lines) as isize {
                            // but not on this one, so start a new one
                            let newline = steal_trailing_wsp(&mut lines);
                            lines.push(newline);
                        }
                        last_line(&mut lines).push_str(encoded);
                        last_word_is_ew = false;
                        continue;
                    }
                }
                // it doesn't fit on a line by itself: unpack the subparts and wrap them
            }
            if let Tok::List { kids, .. } = part {
                // not a Terminal: each piece individually
                for kid in kids.into_iter().rev() {
                    parts.push_front(kid);
                }
                want_encoding = false;
                continue;
            }
            // a terminal, wrapped as an encoded word, combined with the encoded words before it
            last_ew = fold_as_ew(tstr, &mut lines, last_ew.take(), true, last_word_is_ew);
            last_word_is_ew = true;
            want_encoding = false;
            continue;
        }
        if clen(&tstr) as isize <= maxlen as isize - last_len(&lines) as isize {
            last_line(&mut lines).push_str(&tstr);
            last_word_is_ew = last_word_is_ew && tstr.trim_matches(is_wsp).is_empty();
            continue;
        }
        // too long to fit: a major syntactic break, so try it on the next line by itself
        if clen(&tstr) < maxlen {
            let newline = steal_trailing_wsp(&mut lines);
            if !newline.is_empty() || part.startswith_fws() {
                lines.push(format!("{newline}{tstr}"));
                last_word_is_ew = last_word_is_ew && lines.last().is_some_and(|l| l.trim_matches(is_wsp).is_empty());
                last_ew = None;
                continue;
            }
        }
        if let Tok::List { .. } = part {
            // not a terminal: fold the subparts
            let blocked = !part.as_ew_allowed();
            let Tok::List { kids, .. } = part else { continue };
            if blocked {
                wrap_as_ew_blocked += 1;
                parts.push_front(Tok::EndBlocked);
            }
            for kid in kids.into_iter().rev() {
                parts.push_front(kid);
            }
            continue;
        }
        if part.as_ew_allowed() && wrap_as_ew_blocked == 0 {
            // it needs no encoding, but is encoded anyway so it can wrap
            parts.push_front(part);
            want_encoding = true;
            continue;
        }
        // no way to wrap it
        let newline = steal_trailing_wsp(&mut lines);
        if !newline.is_empty() || part.startswith_fws() {
            lines.push(format!("{newline}{tstr}"));
        } else {
            // nor to fold it onto the next line
            last_line(&mut lines).push_str(&tstr);
        }
        last_word_is_ew = last_word_is_ew && tstr.trim_matches(is_wsp).is_empty();
    }
    format!("{}\r\n", lines.join("\r\n"))
}

/// `BaseHeader.fold`: the label, a blank when there is a value, the value's parse tree; the final CRLF dropped.
fn fold_header(name: &str, tree: Tok) -> String {
    let label = Tok::fixed(vec![Tok::value(name, "header-name"), Tok::value(":", "header-sep")], false);
    let mut parts = vec![label];
    if matches!(&tree, Tok::List { kids, .. } if !kids.is_empty()) {
        parts.push(Tok::list(vec![Tok::space(" ", "fws")]));
    }
    parts.push(tree);
    let folded = refold(&parts);
    folded.strip_suffix("\r\n").unwrap_or(&folded).to_string()
}

/// `get_unstructured(value)`: runs of blanks (fws, which also take the Unicode spaces after a blank, as
/// `str.lstrip` does) and the words between them (utext).
fn get_unstructured(value: &str) -> Tok {
    let mut kids = Vec::new();
    let mut rest = value;
    while let Some(first) = rest.chars().next() {
        if is_wsp(first) {
            let after = rest.trim_start_matches(eve_envfile::is_py_space);
            kids.push(Tok::space(&rest[..rest.len() - after.len()], "fws"));
            rest = after;
            continue;
        }
        let end = rest.find(is_wsp).unwrap_or(rest.len());
        kids.push(Tok::value(&rest[..end], "utext"));
        rest = &rest[end..];
    }
    Tok::list(kids)
}

/// An unstructured header (Subject) folded as the SMTP policy folds it. A file name holding a literal RFC 2047
/// encoded word (`=?utf-8?q?...?=`) is written as it is: Python's parser would decode it first.
fn unstructured_header(name: &str, value: &str) -> String {
    fold_header(name, get_unstructured(value))
}

/// `get_dot_atom_text`: atext runs and the dots between them, or None when `value` is not one.
fn dot_atom_text(value: &str) -> Option<Tok> {
    let atom_end = |c: char| "()<>@,:;.\\\"[] \t".contains(c);
    if value.is_empty() || value.starts_with("=?") {
        return None;
    }
    let mut kids = Vec::new();
    for (i, atext) in value.split('.').enumerate() {
        if atext.is_empty() || atext.contains(atom_end) {
            return None;
        }
        if i > 0 {
            kids.push(Tok::value(".", "dot"));
        }
        kids.push(Tok::value(atext, "atext"));
    }
    Some(Tok::fixed(kids, true))
}

/// `get_addr_spec` for `local@domain` with both parts dot-atoms: AddrSpec, LocalPart and Domain around them.
fn addr_spec(value: &str) -> Option<Tok> {
    let (local, domain) = value.split_once('@')?;
    let local = dot_atom_text(local)?;
    let domain = dot_atom_text(domain)?;
    Some(Tok::fixed(
        vec![
            Tok::fixed(vec![Tok::list(vec![local])], false),
            Tok::value("@", "address-at-symbol"),
            Tok::fixed(vec![Tok::list(vec![domain])], false),
        ],
        false,
    ))
}

/// `get_address_list(value)` for one plain address (what an email address in Contacts or a handle the model
/// wrote is): `local@domain`, or `<local@domain>` (a NameAddr of an AngleAddr), both parts dot-atoms. None for
/// any other form.
fn plain_address(value: &str) -> Option<Tok> {
    let mailbox = match value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) {
        Some(inner) => {
            let angle = Tok::list(vec![
                Tok::value("<", "angle-addr-start"),
                addr_spec(inner)?,
                Tok::value(">", "angle-addr-end"),
            ]);
            Tok::list(vec![Tok::list(vec![angle])])
        }
        None => Tok::list(vec![addr_spec(value)?]),
    };
    Some(Tok::list(vec![Tok::list(vec![mailbox])]))
}

/// An address header (To) as the SMTP policy folds it. A plain `local@domain` (or `<local@domain>`) goes through
/// the parse tree Python builds for it. Any other form (a display name, a part that is not a dot-atom such as a
/// quoted local part or `a..b`, a comment, a group, a domain literal) is written as given when it fits, else on a folded line, with a part
/// holding non-ASCII characters as an encoded word: Python's parser would repair such a value (close a quote or
/// a group) before writing it. Neither a Contacts address nor a handle `is_email` passes takes those forms.
fn address_header(name: &str, value: &str) -> String {
    if let Some(tree) = plain_address(value) {
        return fold_header(name, tree);
    }
    let shown = if value.is_ascii() {
        value.to_string()
    } else {
        let mut out = String::new();
        let mut atom = String::new();
        let flush = |atom: &mut String, out: &mut String| {
            if atom.is_ascii() {
                out.push_str(atom);
            } else {
                out.push_str(&encoded_word(atom, prefer_q(atom)));
            }
            atom.clear();
        };
        for c in value.chars() {
            if c == '@' || c == '.' {
                flush(&mut atom, &mut out);
                out.push(c);
            } else {
                atom.push(c);
            }
        }
        flush(&mut atom, &mut out);
        out
    };
    let line = format!("{name}: {shown}");
    if line.len() <= MAIL_LINE {
        line
    } else {
        format!("{name}:\r\n {shown}")
    }
}

/// urllib's `quote(text, safe="")`: what an RFC 2231 parameter value carries.
fn quote_2231(c: char) -> String {
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf)
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `Content-Disposition: attachment; filename=...` as the SMTP policy writes it: a quoted string for an ASCII
/// name, RFC 2231 (`filename*=utf-8''...`) for any other, folded, and split into numbered sections when one
/// line cannot hold it.
fn disposition_header(filename: &str) -> String {
    let head = "Content-Disposition: attachment;";
    if filename.is_empty() {
        // `_formatparam` writes a parameter with an empty value as its bare name
        return format!("{head} filename");
    }
    let (param, charset) = if filename.is_ascii() {
        (format!("filename=\"{}\"", filename.replace('\\', "\\\\").replace('"', "\\\"")), "us-ascii")
    } else {
        (format!("filename*=utf-8''{}", filename.chars().map(quote_2231).collect::<String>()), "utf-8")
    };
    // `_fold_mime_parameters`: on this line while it stays under the limit, else alone on the next when it fits
    if head.len() + param.len() + 1 < MAIL_LINE {
        return format!("{head} {param}");
    }
    if param.len() + 2 <= MAIL_LINE {
        return format!("{head}\r\n {param}");
    }
    let mut sections: Vec<String> = Vec::new();
    let mut line = format!(" filename*0*={charset}''");
    let mut has_piece = false;
    for c in filename.chars() {
        let piece = quote_2231(c);
        if has_piece && line.len() + piece.len() + 1 > MAIL_LINE {
            sections.push(std::mem::take(&mut line));
            line = format!(" filename*{}*=", sections.len());
        }
        line.push_str(&piece);
        has_piece = true;
    }
    sections.push(line);
    format!("{head}\r\n{}", sections.join(";\r\n"))
}

/// email.generator's `_make_boundary`: fifteen `=`, nineteen random digits, `==`; never a line of the text.
fn make_boundary(texts: &[&str]) -> String {
    let token = rand::random::<u64>() >> 1;
    let base = format!("==============={token:019}==");
    let mut boundary = base.clone();
    let mut counter = 0;
    while texts.iter().any(|t| t.lines().any(|l| l.trim_end_matches(eve_envfile::is_py_space) == format!("--{boundary}") || l.trim_end_matches(eve_envfile::is_py_space) == format!("--{boundary}--"))) {
        boundary = format!("{base}.{counter}");
        counter += 1;
    }
    boundary
}

/// py: share.py:733 `def attachment_mail(to: str, filename: str, note: str, data: bytes) -> bytes:`
///
/// One RFC 2822 email: To, Subject (the file's name), the note as the body, the file attached by its type.
/// From is left out: Gmail fills in the signed-in account. As Python's `EmailMessage(policy=SMTP)` writes it.
pub fn attachment_mail(to: &str, filename: &str, note: &str, data: &[u8]) -> Result<Vec<u8>, MailError> {
    if to.chars().any(breaks_line) || filename.chars().any(breaks_line) {
        return Err(MailError);
    }
    let (cte, text) = encode_text(note);
    let text = crlf(&text);
    let attached = crlf(&encode_base64_lines(data, MAIL_LINE));
    let mime = eve_link_uploads::mimetypes::guess_type(filename).0.unwrap_or_else(|| "application/octet-stream".into());
    let (main, sub) = match mime.split_once('/') {
        Some((main, sub)) if !sub.is_empty() => (main.to_string(), sub.to_string()),
        Some((main, _)) => (main.to_string(), "octet-stream".to_string()),
        None => (mime.clone(), "octet-stream".to_string()),
    };
    let boundary = make_boundary(&[&text, &attached]);
    let mut out = String::new();
    out.push_str(&address_header("To", to));
    out.push_str("\r\n");
    out.push_str(&unstructured_header("Subject", filename));
    out.push_str("\r\nMIME-Version: 1.0\r\n");
    out.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"));
    out.push_str(&format!("--{boundary}\r\n"));
    out.push_str(&format!("Content-Type: text/plain; charset=\"utf-8\"\r\nContent-Transfer-Encoding: {cte}\r\n\r\n"));
    out.push_str(&text);
    out.push_str(&format!("\r\n--{boundary}\r\n"));
    out.push_str(&format!("Content-Type: {main}/{sub}\r\nContent-Transfer-Encoding: base64\r\n"));
    out.push_str(&disposition_header(filename));
    out.push_str("\r\nMIME-Version: 1.0\r\n\r\n");
    out.push_str(&attached);
    out.push_str(&format!("\r\n--{boundary}--\r\n"));
    Ok(out.into_bytes())
}

/// py: share.py:749 `def gmail_can_send(environ: Optional[Mapping[str, str]] = None) -> bool:`
///
/// Whether Gmail may send: the apps policy lets it, and the stored sign-in granted gmail.send (a file check,
/// no network). A sign-in from before 2026-10-09 has no gmail.send, so email shares stay Drive links until the
/// owner signs in again.
pub fn gmail_can_send(environ: Option<&Environ>) -> bool {
    let cfg = eve_apps_mcp::configured(environ);
    if !cfg.enabled || !eve_apps_mcp::google_scopes(&cfg.policy).iter().any(|s| s == GMAIL_SEND_SCOPE) {
        return false;
    }
    let granted = str_or_empty(eve_apps_mcp::read_json(&cfg.google_token_path()).get("scope"));
    granted.split(eve_envfile::is_py_space).filter(|w| !w.is_empty()).any(|s| s == GMAIL_SEND_SCOPE)
}

/// py: share.py:762 `def _chunks(path: Path) -> Iterator[bytes]:`
///
/// The file in CHUNK-sized blocks.
pub(crate) fn chunks(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut f = std::fs::File::open(path)?;
    let mut out = Vec::new();
    loop {
        let mut block = vec![0u8; CHUNK as usize];
        let mut filled = 0;
        while filled < block.len() {
            let n = f.read(&mut block[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            return Ok(out);
        }
        block.truncate(filled);
        out.push(block);
    }
}

/// py: share.py:771 `def make_share(environ: Optional[Mapping[str, str]] = None, token: Optional[Callable[[], Optional[str]]] = None) -> Optional[Share]:`
///
/// The share tools, or None when off (then no tool is lent). ``token`` (apps_mcp.TokenSource("google", ...))
/// turns email on; lend it only when the Google sign-in has Drive's write scope.
pub fn make_share(environ: Option<&Environ>, token: Option<TokenFn>) -> Option<Share> {
    let cfg = configured(environ, PLATFORM);
    if !cfg.enabled {
        tracing::info!("share: off (EVE_SHARE)");
        return None;
    }
    if !cfg.imessage && token.is_none() {
        tracing::info!("share: on, but neither iMessage (macOS) nor Google is here; off");
        return None;
    }
    let gmail = token.is_some() && gmail_can_send(environ);
    tracing::info!(
        "share: on (imessage={}, email={}, attached={})",
        py_bool(cfg.imessage),
        py_bool(token.is_some()),
        py_bool(gmail)
    );
    Some(Share::new(cfg).with_token(token).with_gmail(gmail))
}

/// py: share.py:787 `def google_token(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[], Optional[str]]]:`
///
/// The direct apps' Google token when it can write to Drive (drive.file), else None.
pub fn google_token(environ: Option<&Environ>) -> Option<TokenFn> {
    let cfg = eve_apps_mcp::configured(environ);
    if !cfg.enabled {
        return None;
    }
    if !eve_apps_mcp::google_scopes(&cfg.policy).iter().any(|s| s == "https://www.googleapis.com/auth/drive.file") {
        return None;
    }
    let src = Arc::new(eve_apps_mcp::TokenSource::new("google", cfg));
    if !src.available() {
        return None;
    }
    Some(Arc::new(move || {
        let src = src.clone();
        async move { src.get().await }.boxed()
    }))
}

/// `Ask`: `ask(question) -> the owner's answer` (the inlet's firm yes/no).
pub type Ask = Arc<dyn Fn(String) -> BoxFuture<'static, String> + Send + Sync>;
/// `token() -> access token | None` (apps_mcp.TokenSource for Google, which may refresh over the network).
pub type TokenFn = Arc<dyn Fn() -> BoxFuture<'static, Option<String>> + Send + Sync>;
/// py: share.py:110 `Runner = Callable[[list[str], bytes, float], Awaitable[tuple[int, str, str]]]`: (argv, stdin,
/// timeout) → (exit code, stdout, stderr); osascript_runner, or a test's fake.
pub type Runner = Arc<dyn Fn(Vec<String>, Vec<u8>, f64) -> BoxFuture<'static, (i64, String, String)> + Send + Sync>;
