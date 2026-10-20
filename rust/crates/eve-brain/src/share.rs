//! A port of `share.py`, the whole module.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;

// py: share.py:79 `log` (Logger): ported by this file's unit.

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

// py: share.py:98 `OUTBOX` (PosixPath): ported by this file's unit.

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
pub const RC_TIMEOUT: f64 = 124.0;

/// py: share.py:107 `RC_NO_START`
pub const RC_NO_START: i64 = 127;

// py: share.py:110 `Runner` (_CallableGenericAlias): ported by this file's unit.

// py: share.py:111 `Ask` (_CallableGenericAlias): ported by this file's unit.

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
/// The real runner: one process, an argv list (never a shell), the script on stdin, a timeout.
pub async fn osascript_runner(argv: &[String], stdin: &[u8], timeout: f64) -> (i64, String, String) {
    todo!("share.py:148 osascript_runner")
}

/// py: share.py:166 `def _kill(proc: asyncio.subprocess.Process) -> None:`
pub(crate) fn kill(proc: &mut tokio::process::Child) {
    todo!("share.py:166 _kill")
}

/// py: share.py:175 `_EMAIL`
/// Python flags 32.
pub(crate) static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[^@\s]+@[^@\s]+\.[^@\s]+(?:\n?\z)"#).expect("EMAIL"));

/// py: share.py:176 `_PHONE`
/// Python flags 32.
pub(crate) static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\+?[\d\s().\-]{7,20}(?:\n?\z)"#).expect("PHONE"));

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
    todo!("share.py:181 is_email")
}

/// py: share.py:185 `def is_phone(text: str) -> bool:`
pub fn is_phone(text: &str) -> bool {
    todo!("share.py:185 is_phone")
}

/// py: share.py:190 `def phone_handle(text: str) -> str:`
///
/// "+1 (555) 010-0199" → "+15550100199": what Messages takes as a handle.
pub fn phone_handle(text: &str) -> String {
    todo!("share.py:190 phone_handle")
}

/// py: share.py:196 `def _label(raw: Any) -> str:`
pub(crate) fn label(raw: &Value) -> String {
    todo!("share.py:196 _label")
}

/// py: share.py:202 `def human_size(n: int) -> str:`
pub fn human_size(n: i64) -> String {
    todo!("share.py:202 human_size")
}

/// py: share.py:210 `def resolve_file(raw: str, home: Optional[Path] = None) -> tuple[Optional[Path], str]:`
///
/// The owner-path rules of telegram.send_file: a regular file in the owner's home, no hidden folder.
pub fn resolve_file(raw: &str, home: Option<&Path>) -> (Option<PathBuf>, String) {
    todo!("share.py:210 resolve_file")
}

/// py: share.py:225 `class ShareConfig:`
#[derive(Debug, Clone)]
pub struct ShareConfig {
    pub enabled: bool,
    pub imessage: bool,
}

/// py: share.py:230 `def configured(environ: Optional[Mapping[str, str]] = None, platform: str = sys.platform) -> ShareConfig:`
pub fn configured(environ: Option<&Environ>, platform: &str) -> ShareConfig {
    todo!("share.py:230 configured")
}

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
#[derive(Debug, Clone)]
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

/// py: share.py:297 `def _open_exact(path: Path) -> int:`
///
/// A read-only descriptor on ``path`` itself, walked from the root one component at a time with O_NOFOLLOW:
/// a link anywhere on the way is refused (ELOOP), never followed. ``path`` is the real path the owner-path
/// rules judged, so it holds no link. O_NOFOLLOW on the whole path checks only its last component: a folder
/// swapped for a link to ~/.ssh between the resolve and the open was followed, and the hidden file was asked
/// about under the innocent name and sent (Lean ShareGate P4, 2026-10-05).
pub(crate) fn open_exact(path: &Path) -> i64 {
    todo!("share.py:297 _open_exact")
}

/// py: share.py:321 `def fingerprint(path: Path) -> tuple[os.stat_result, str]:`
///
/// (fstat, SHA-256 of the bytes) of the regular file at ``path``, read through one O_NOFOLLOW descriptor.
///
/// Rust: the stat is (size, mtime_ns, inode) — what Python compares of the `os.stat_result`.
pub fn fingerprint(path: &Path) -> std::io::Result<((u64, i128, u64), String)> {
    todo!("share.py:321 fingerprint")
}

/// py: share.py:334 `def stage_exact(p: Pending, dest: Path) -> None:`
///
/// Copy the approved bytes to ``dest`` (created new, 0600) through one O_NOFOLLOW descriptor, checking the
/// inode, device, size, mtime and SHA-256 the question saw; on any difference ``dest`` is removed and
/// FileChanged raised. What is sent is this copy, never the path opened again (review, 2026-10-05).
pub fn stage_exact(p: &Pending, dest: &Path) {
    todo!("share.py:334 stage_exact")
}

/// py: share.py:359 `class Share:`
///
/// The share tools. ``runner`` runs osascript; ``token`` gives a Google access token (apps_mcp.TokenSource)
/// or None; ``http`` is the Drive client (tests lend an httpx.MockTransport).
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
    pub(crate) pending: HashMap<String, Pending>,
    pub(crate) folder_id: String,
}

impl Share {
    /// py: share.py:376 `def email_on(self) -> bool:`
    pub fn email_on(&self) -> bool {
        todo!("share.py:376 Share.email_on")
    }

    /// py: share.py:379 `def tools(self) -> list[dict[str, Any]]:`
    pub fn tools(&self) -> Vec<Obj> {
        todo!("share.py:379 Share.tools")
    }

    /// py: share.py:387 `def instructions(self) -> str:`
    pub fn instructions(&self) -> String {
        todo!("share.py:387 Share.instructions")
    }

    /// py: share.py:396 `async def handle(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:`
    pub async fn handle(&self, name: &str, args: &Obj) -> Obj {
        todo!("share.py:396 Share.handle")
    }

    /// py: share.py:406 `async def ask_and_share(self, args: Mapping[str, Any], ask: Ask) -> dict[str, Any]:`
    ///
    /// The whole share_file flow for a caller with a yes/no prompt: the question, the owner's answer, the send.
    /// The token never reaches the model.
    pub async fn ask_and_share(&self, args: &Obj, ask: Ask) -> Obj {
        todo!("share.py:406 Share.ask_and_share")
    }

    /// py: share.py:426 `async def lookup(self, name: str) -> tuple[Optional[list[dict[str, Any]]], int, str]:`
    ///
    /// (people, how many matched, why not). People carry name, phones and emails as [{label, value}].
    pub async fn lookup(&self, name: &str) -> (Option<Vec<Obj>>, i64, String) {
        todo!("share.py:426 Share.lookup")
    }

    /// py: share.py:455 `async def find_contact(self, name: str) -> dict[str, Any]:`
    pub async fn find_contact(&self, name: &str) -> Obj {
        todo!("share.py:455 Share.find_contact")
    }

    /// py: share.py:464 `async def _recipient(self, to: str, via: str) -> dict[str, Any]:`
    ///
    /// {"handle", "who"} or {"ok": False, ...} (with ``matches`` when the owner must pick).
    pub(crate) async fn recipient(&self, to: &str, via: &str) -> Obj {
        todo!("share.py:464 Share._recipient")
    }

    /// py: share.py:504 `async def prepare(self, raw_path: str, to: str, via: str, note: str) -> dict[str, Any]:`
    pub async fn prepare(&self, raw_path: &str, to: &str, via: &str, note: &str) -> Obj {
        todo!("share.py:504 Share.prepare")
    }

    /// py: share.py:543 `def _sweep_pending(self) -> None:`
    pub(crate) fn sweep_pending(&self) {
        todo!("share.py:543 Share._sweep_pending")
    }

    /// py: share.py:548 `def cancel(self, token: str) -> None:`
    pub fn cancel(&self, token: &str) {
        todo!("share.py:548 Share.cancel")
    }

    /// py: share.py:552 `async def confirm(self, token: str) -> dict[str, Any]:`
    ///
    /// Send what the question described, once. Nothing runs for an unknown, used or expired token, or when
    /// the file changed since the question.
    pub async fn confirm(&self, token: &str) -> Obj {
        todo!("share.py:552 Share.confirm")
    }

    /// py: share.py:569 `async def _send_imessage(self, p: Pending) -> dict[str, Any]:`
    pub(crate) async fn send_imessage(&self, p: &Pending) -> Obj {
        todo!("share.py:569 Share._send_imessage")
    }

    /// py: share.py:600 `def _sweep_outbox(self, outbox: Path) -> None:`
    pub(crate) fn sweep_outbox(&self, outbox: &Path) {
        todo!("share.py:600 Share._sweep_outbox")
    }

    /// py: share.py:610 `def _send_gmail(self, p: Pending) -> dict[str, Any]:`
    ///
    /// The approved bytes, staged, as one email's attachment from the owner's Gmail.
    pub(crate) fn send_gmail(&self, p: &Pending) -> Obj {
        todo!("share.py:610 Share._send_gmail")
    }

    /// py: share.py:639 `def _drive(self, method: str, url: str, **kw: Any) -> httpx.Response:`
    /// (Python also takes **kw.)
    pub(crate) async fn drive(&self, method: &str, url: &str, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        todo!("share.py:639 Share._drive")
    }

    /// py: share.py:658 `def _folder(self) -> str:`
    pub(crate) fn folder(&self) -> String {
        todo!("share.py:658 Share._folder")
    }

    /// py: share.py:674 `def _upload(self, path: Path, size: int, folder: str) -> dict[str, Any]:`
    pub(crate) fn upload(&self, path: &Path, size: i64, folder: &str) -> Obj {
        todo!("share.py:674 Share._upload")
    }

    /// py: share.py:692 `def _send_drive(self, p: Pending) -> dict[str, Any]:`
    pub(crate) fn send_drive(&self, p: &Pending) -> Obj {
        todo!("share.py:692 Share._send_drive")
    }

    /// py: share.py:706 `def _send_drive_staged(self, p: Pending, staged: Path) -> dict[str, Any]:`
    pub(crate) fn send_drive_staged(&self, p: &Pending, staged: &Path) -> Obj {
        todo!("share.py:706 Share._send_drive_staged")
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

/// py: share.py:733 `def attachment_mail(to: str, filename: str, note: str, data: bytes) -> bytes:`
///
/// One RFC 2822 email: To, Subject (the file's name), the note as the body, the file attached by its type.
/// From is left out: Gmail fills in the signed-in account.
pub fn attachment_mail(to: &str, filename: &str, note: &str, data: &[u8]) -> Vec<u8> {
    todo!("share.py:733 attachment_mail")
}

/// py: share.py:749 `def gmail_can_send(environ: Optional[Mapping[str, str]] = None) -> bool:`
///
/// Whether Gmail may send: the apps policy lets it, and the stored sign-in granted gmail.send (a file check,
/// no network). A sign-in from before 2026-10-09 has no gmail.send, so email shares stay Drive links until the
/// owner signs in again.
pub fn gmail_can_send(environ: Option<&Environ>) -> bool {
    todo!("share.py:749 gmail_can_send")
}

/// py: share.py:762 `def _chunks(path: Path) -> Iterator[bytes]:`
pub(crate) fn chunks(path: &Path) -> Vec<Vec<u8>> {
    todo!("share.py:762 _chunks")
}

/// py: share.py:771 `def make_share(environ: Optional[Mapping[str, str]] = None, token: Optional[Callable[[], Optional[str]]] = None) -> Optional[Share]:`
///
/// The share tools, or None when off (then no tool is lent). ``token`` (apps_mcp.TokenSource("google", ...))
/// turns email on; lend it only when the Google sign-in has Drive's write scope.
pub fn make_share(environ: Option<&Environ>, token: Option<TokenFn>) -> Option<Share> {
    todo!("share.py:771 make_share")
}

/// py: share.py:787 `def google_token(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[], Optional[str]]]:`
///
/// The direct apps' Google token when it can write to Drive (drive.file), else None.
pub fn google_token(environ: Option<&Environ>) -> Option<TokenFn> {
    todo!("share.py:787 google_token")
}

/// `Ask`: `ask(question) -> the owner's answer` (the inlet's firm yes/no).
pub type Ask = Arc<dyn Fn(String) -> futures::future::BoxFuture<'static, String> + Send + Sync>;
/// `token() -> access token | None` (apps_mcp.TokenSource for Google).
pub type TokenFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
/// py: share.py:110 `Runner = Callable[[list[str], bytes, float], Awaitable[tuple[int, str, str]]]`: (argv, stdin,
/// timeout) → (exit code, stdout, stderr); osascript_runner, or a test's fake.
pub type Runner =
    Arc<dyn Fn(Vec<String>, Vec<u8>, f64) -> futures::future::BoxFuture<'static, (i64, String, String)> + Send + Sync>;
