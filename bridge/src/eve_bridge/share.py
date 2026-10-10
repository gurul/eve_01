"""Share a file of the owner's with someone: by iMessage (Messages on this Mac) or by email (a Google Drive share).

The owner asked on 2026-10-04: "cant file sending be optimized through cli … instead of computer use". Until
then "send the report to Ana" was a computer task: Codex drove Messages or Mail on the screen, which took
minutes. This module does it with two command-line calls and one HTTP round trip, in seconds.

Two tools for the chat's brain:

* ``find_contact`` {name}: the people in the owner's Contacts whose name contains the words, with their phone
  numbers and email addresses (read only, no approval). The brain uses it to pick the right person or handle
  before it asks the owner anything.
* ``share_file`` {path, to, via, message}: ``via`` is ``imessage`` or ``email``; ``to`` is a contact's name, a
  phone number or an email address; ``message`` is an optional short note.

**Consent.** ``handle("share_file", ...)`` never sends. It checks the file and the recipient and returns
``{"needs_approval": True, "question": ..., "token": ...}``. The send happens only through ``confirm(token)``,
and only while the file is the one the question showed: the same real path, inode, size, mtime and SHA-256, read
through a descriptor that never follows a link. What is sent is a copy made from that descriptor, never the path
opened again. The recipient's exact handle is frozen in the token, so what is sent is what the owner approved. A token is good once, for ``APPROVAL_TTL_SECS``; a denied,
unknown or expired token runs nothing. ``ask_and_share(args, ask)`` is the whole flow for a caller with a yes/no
prompt (telegram's ``_ask_user``).

**iMessage.** The recipient is resolved with Contacts through JXA (``osascript -l JavaScript``, JSON out); a raw
phone number or email address skips the lookup. The send is an AppleScript to Messages: the note (if any), then
the file. Every value (the name, the handle, the path, the note) travels as an ``on run argv`` argument and the
script itself comes in on stdin, so no value is ever part of a script's text (no injection) and none starts with
a dash where osascript would read it as an option. osascript's exit code says Messages took the message, not
that it was delivered: the answer says so.

Messages is sandboxed. On macOS 15 and later a file sent by AppleScript from outside the folders it may read
(e.g. ~/Documents, ~/Downloads) shows "Not Delivered" while osascript exits 0; text still goes. Reported in
https://www.macscripter.net/t/sequoia-messages-send-file/76468 ("the pictures folder allows the file to be
sent… no other folder seems to work") and https://github.com/anthropics/claude-plugins-official/issues/1113
(the sandbox reads ~/Library/Messages, ~/Library/SMS, ~/Media and its caches). ~/Library/Messages is behind
Full Disk Access for every other process, so eve copies the file into a folder of its own,
``~/Pictures/eve-outbox``, and sends the copy. Copies older than ``OUTBOX_KEEP_SECS`` are swept on each send
(Messages has copied the file into its own attachments by then).

**Email, attached** (owner, 2026-10-09: "the chat path should be able to send files"). When Gmail may send (the
apps policy is not read only, and the sign-in granted gmail.send), the file goes as a real attachment of an email
from the owner's Gmail: the approved bytes are staged exactly as for iMessage, built into one RFC 2822 message
(To, Subject = the file's name, the note as the body, the file as its MIME type) and posted to Gmail's media
upload send (``upload/gmail/v1/users/me/messages/send?uploadType=media``, ``message/rfc822``). Gmail delivers 25 MB
at most and base64 adds a third, so a file over ``MAX_GMAIL_ATTACH_BYTES`` goes as a Drive link instead; the
question says which.

**Email, as a link.** Before 2026-10-09 Gmail was read only, so every email share was this. The file is uploaded to the owner's Drive
(Drive v3 files.create: multipart up to 5 MB, resumable above; the ``drive.file`` scope eve already has lets it
create files and manage their permissions) into a "eve shared" folder eve made, and then given one reader
permission with ``sendNotificationEmail=true`` and the note as ``emailMessage``
(https://developers.google.com/drive/api/reference/rest/v3/permissions/create), so Google emails the
recipient a link from the owner's account. The answer carries the file's ``webViewLink``.

``EVE_SHARE=1`` turns it on (default off). iMessage needs macOS; email needs the Google sign-in of the
direct apps with Drive's write scope. Logs carry sizes, counts and exit codes only: never a file name, a
recipient or a note.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import mimetypes
import os
import re
import secrets
import stat
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable, Iterator, Mapping, Optional

import httpx

log = logging.getLogger(__name__)

OFF_REASON = "Sharing files by iMessage or email is off on this computer (EVE_SHARE=1 turns it on)"
TOOL_NAMES = ("share_file", "find_contact")
VIAS = ("imessage", "email")

APPROVAL_TTL_SECS = 600.0
CONTACTS_SECS = 25.0              # the first Contacts read of a session launches Contacts
SEND_SECS = 30.0
DRIVE_SECS = 120.0
MAX_IMESSAGE_BYTES = 100 * 1024 * 1024     # iMessage's attachment ceiling
MAX_DRIVE_BYTES = 1024 * 1024 * 1024       # a cap of eve's, not Drive's: an upload this big is a mistake
MULTIPART_MAX = 5 * 1024 * 1024            # Drive: multipart is for 5 MB or less, resumable above
MAX_GMAIL_ATTACH_BYTES = 18 * 1024 * 1024  # Gmail delivers 25 MB at most; base64 makes 18 MB about 24.6 MB
GMAIL_SEND_UPLOAD = "https://gmail.googleapis.com/upload/gmail/v1/users/me/messages/send"
GMAIL_SEND_SCOPE = "https://www.googleapis.com/auth/gmail.send"
CHUNK = 1024 * 1024
MAX_MATCHES = 8
MAX_NOTE = 500
OUTBOX = Path("~/Pictures/eve-outbox")
OUTBOX_KEEP_SECS = 3600.0
FOLDER_NAME = "eve shared"
FOLDER_MIME = "application/vnd.google-apps.folder"
FOLDER_PROP = ("eve", "share-folder")    # appProperties marking the folder eve made
DRIVE = "https://www.googleapis.com/drive/v3"
DRIVE_UPLOAD = "https://www.googleapis.com/upload/drive/v3/files"
FILE_FIELDS = "id,name,size,webViewLink"
RC_TIMEOUT = 124
RC_NO_START = 127

# (argv, stdin, timeout) -> (rc, stdout, stderr). A timeout is an answer (RC_TIMEOUT), never an exception.
Runner = Callable[[list[str], bytes, float], Awaitable[tuple[int, str, str]]]
Ask = Callable[[str], Awaitable[str]]


# ---- the scripts (values only ever arrive as argv) ------------------------------------------------------------

CONTACTS_JXA = """function run(argv) {
  var q = argv[0];
  var max = parseInt(argv[1], 10);
  var people = Application("Contacts").people.whose({name: {_contains: q}})();
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
"""

MESSAGES_SEND = """on run argv
  set theHandle to item 1 of argv
  set theFile to POSIX file (item 2 of argv)
  set theNote to item 3 of argv
  tell application "Messages"
    set theService to 1st account whose service type = iMessage
    set theEve to participant theHandle of theService
    if theNote is not "" then send theNote to theEve
    send theFile to theEve
  end tell
end run
"""


async def osascript_runner(argv: list[str], stdin: bytes, timeout: float) -> tuple[int, str, str]:
    """The real runner: one process, an argv list (never a shell), the script on stdin, a timeout."""
    try:
        proc = await asyncio.create_subprocess_exec(*argv, stdin=asyncio.subprocess.PIPE,
                                                    stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    except OSError as e:
        return RC_NO_START, "", f"could not start ({e.strerror or e})"
    try:
        out, err = await asyncio.wait_for(proc.communicate(stdin), timeout)
    except asyncio.TimeoutError:
        _kill(proc)
        return RC_TIMEOUT, "", ""
    except BaseException:
        _kill(proc)
        raise
    return proc.returncode or 0, out.decode("utf-8", "replace"), err.decode("utf-8", "replace")


def _kill(proc: asyncio.subprocess.Process) -> None:
    try:
        proc.kill()
    except ProcessLookupError:
        pass


# ---- small rules --------------------------------------------------------------------------------------------

_EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")
_PHONE = re.compile(r"^\+?[\d\s().\-]{7,20}$")
_LABEL = re.compile(r"^_\$!<(.*)>!\$_$")       # Contacts' built-in labels: "_$!<Mobile>!$_"
_CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f]")


def is_email(text: str) -> bool:
    return bool(_EMAIL.match(text.strip()))


def is_phone(text: str) -> bool:
    t = text.strip()
    return bool(_PHONE.match(t)) and sum(c.isdigit() for c in t) >= 7


def phone_handle(text: str) -> str:
    """"+1 (555) 010-0199" → "+15550100199": what Messages takes as a handle."""
    t = text.strip()
    return ("+" if t.startswith("+") else "") + "".join(c for c in t if c.isdigit())


def _label(raw: Any) -> str:
    text = str(raw or "")
    m = _LABEL.match(text)
    return (m.group(1) if m else text).strip().lower()


def human_size(n: int) -> str:
    if n < 1024:
        return f"{n} bytes"
    if n < 1024 * 1024:
        return f"{n / 1024:.0f} KB"
    return f"{n / (1024 * 1024):.1f} MB"


def resolve_file(raw: str, home: Optional[Path] = None) -> tuple[Optional[Path], str]:
    """The owner-path rules of telegram.send_file: a regular file in the owner's home, no hidden folder."""
    from .telegram import resolve_owner_path  # lazy: telegram imports this module

    real, why = resolve_owner_path(raw, home)
    if real is None:
        return None, why
    if real.is_dir():
        return None, "that is a folder; name a file in it (list_files shows them)"
    if not real.is_file():
        return None, "not a regular file"
    return real, ""


@dataclass(frozen=True)
class ShareConfig:
    enabled: bool
    imessage: bool


def configured(environ: Optional[Mapping[str, str]] = None, platform: str = sys.platform) -> ShareConfig:
    env = os.environ if environ is None else environ
    on = (env.get("EVE_SHARE") or "").strip().lower() in ("1", "true", "yes", "on")
    return ShareConfig(enabled=on, imessage=on and platform == "darwin")


# ---- tools --------------------------------------------------------------------------------------------------

EMAIL_AS_LINK = "by email as a Google Drive link Google emails them"
EMAIL_ATTACHED = "by email, attached, from the owner's Gmail (a file over 18 MB goes as a Google Drive link)"
VIA_EMAIL_LINK = "via email to send a Google Drive link that Google emails them"
VIA_EMAIL_ATTACHED = "via email to email it as an attachment from the owner's Gmail"

TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": "share_file", "strict": True,
     "description": "Send one of the owner's files to someone: by iMessage, or " + EMAIL_AS_LINK + ". The owner is asked yes/no first; you don't ask. `to` is a contact's "
                    "name, a phone number or an email address. If two people match, you get them back: ask the "
                    "owner which, then call again with that person's number or address.",
     "parameters": {"type": "object", "additionalProperties": False, "required": ["path", "to", "via", "message"],
                    "properties": {
                        "path": {"type": "string",
                                 "description": "The file's path in the owner's home (~/Documents/report.pdf)."},
                        "to": {"type": "string",
                               "description": "A name from the owner's Contacts, a phone number or an email "
                                              "address."},
                        "via": {"type": "string", "enum": list(VIAS)},
                        "message": {"type": ["string", "null"],
                                    "description": "A short note sent with the file; or null."}}}},
    {"type": "function", "name": "find_contact", "strict": True,
     "description": "Look a person up in the owner's Contacts: everyone whose name contains the words, with "
                    "their phone numbers and email addresses. Read only.",
     "parameters": {"type": "object", "additionalProperties": False, "required": ["name"],
                    "properties": {"name": {"type": "string"}}}},
]

INSTRUCTIONS = """Sharing a file with someone (share_file, find_contact).
To send one of the owner's files to another person, use share_file: via imessage for a phone number or an
iMessage contact, via email to send a Google Drive link that Google emails them. Do not start a computer task for
this. The owner is asked yes/no by eve before anything is sent; do not ask them first yourself.
If share_file returns matches (two people, or one person with several numbers), ask the owner which one and call
again with that number or address. find_contact looks someone up when you only need their details.
An iMessage result means Messages took it, not that it was delivered: say so. An email result has a link: give it.
"Send the report to Ana" is share_file; "send me the report" is send_file, to the owner in this chat."""


@dataclass
class Pending:
    token: str
    via: str
    path: Path             # the real path the question named (resolve_file)
    size: int
    mtime_ns: int
    handle: str            # iMessage: a phone number or an iMessage email; email: the address
    who: str               # the name the owner sees
    note: str
    created: float
    raw: str = ""          # the path as the model wrote it, resolved again at the send
    attach: bool = False   # email: a Gmail attachment (True) or a Drive link (False), as the question said
    ino: int = 0
    dev: int = 0
    sha256: str = ""


class FileChanged(Exception):
    """The file at the send is not the one the owner approved."""


def _open_exact(path: Path) -> int:
    """A read-only descriptor on ``path`` itself, walked from the root one component at a time with O_NOFOLLOW:
    a link anywhere on the way is refused (ELOOP), never followed. ``path`` is the real path the owner-path
    rules judged, so it holds no link. O_NOFOLLOW on the whole path checks only its last component: a folder
    swapped for a link to ~/.ssh between the resolve and the open was followed, and the hidden file was asked
    about under the innocent name and sent (Lean ShareGate P4, 2026-10-05)."""
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    path = Path(path)
    if not path.is_absolute():
        raise FileChanged("not an absolute path")
    parts = path.parts[1:]
    if not parts:
        raise FileChanged("not a file")
    fd = os.open(path.anchor, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0))
    try:
        for part in parts[:-1]:
            nxt = os.open(part, flags | getattr(os, "O_DIRECTORY", 0), dir_fd=fd)
            os.close(fd)
            fd = nxt
        return os.open(parts[-1], flags, dir_fd=fd)
    finally:
        os.close(fd)


def fingerprint(path: Path) -> tuple[os.stat_result, str]:
    """(fstat, SHA-256 of the bytes) of the regular file at ``path``, read through one O_NOFOLLOW descriptor."""
    fd = _open_exact(path)
    with os.fdopen(fd, "rb") as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode):
            raise FileChanged("not a regular file")
        h = hashlib.sha256()
        for block in iter(lambda: f.read(CHUNK), b""):
            h.update(block)
    return st, h.hexdigest()


def stage_exact(p: Pending, dest: Path) -> None:
    """Copy the approved bytes to ``dest`` (created new, 0600) through one O_NOFOLLOW descriptor, checking the
    inode, device, size, mtime and SHA-256 the question saw; on any difference ``dest`` is removed and
    FileChanged raised. What is sent is this copy, never the path opened again (review, 2026-10-05)."""
    fd = _open_exact(p.path)
    with os.fdopen(fd, "rb") as src:
        st = os.fstat(src.fileno())
        if (not stat.S_ISREG(st.st_mode) or (st.st_ino, st.st_dev, st.st_size, st.st_mtime_ns)
                != (p.ino, p.dev, p.size, p.mtime_ns)):
            raise FileChanged("the file changed")
        out = os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
        h = hashlib.sha256()
        try:
            with os.fdopen(out, "wb") as f:
                for block in iter(lambda: src.read(CHUNK), b""):
                    h.update(block)
                    f.write(block)
            if h.hexdigest() != p.sha256:
                raise FileChanged("the file's bytes changed")
        except BaseException:
            dest.unlink(missing_ok=True)
            raise


@dataclass
class Share:
    """The share tools. ``runner`` runs osascript; ``token`` gives a Google access token (apps_mcp.TokenSource)
    or None; ``http`` is the Drive client (tests lend an httpx.MockTransport)."""

    config: ShareConfig
    runner: Runner = osascript_runner
    token: Optional[Callable[[], Optional[str]]] = None
    http: Optional[httpx.Client] = None
    home: Optional[Path] = None
    outbox: Optional[Path] = None
    clock: Callable[[], float] = time.monotonic
    gmail: bool = False    # Gmail may send (gmail_can_send): an email share is an attachment up to its size
    _pending: dict[str, Pending] = field(default_factory=dict)
    _folder_id: str = ""

    # -- what the model gets --
    @property
    def email_on(self) -> bool:
        return self.token is not None

    def tools(self) -> list[dict[str, Any]]:
        tools = json.loads(json.dumps(TOOLS))
        vias = [v for v in VIAS if (v == "imessage" and self.config.imessage) or (v == "email" and self.email_on)]
        tools[0]["parameters"]["properties"]["via"]["enum"] = vias
        if self.gmail:
            tools[0]["description"] = tools[0]["description"].replace(EMAIL_AS_LINK, EMAIL_ATTACHED)
        return tools if self.config.imessage else tools[:1]      # find_contact needs Contacts (macOS)

    def instructions(self) -> str:
        text = INSTRUCTIONS.replace(VIA_EMAIL_LINK, VIA_EMAIL_ATTACHED) if self.gmail else INSTRUCTIONS
        if not self.config.imessage:
            # no Contacts off the Mac: tools() offers share_file alone, so the block names nothing else (the owner's
            # rule for prompts; Lean RoutedTurn P5 assumes the block and the tools agree)
            text = text.replace("Sharing a file with someone (share_file, find_contact).", "Sharing a file with someone (share_file).")
            text = text.replace(" find_contact looks someone up when you only need their details.", "")
        return text

    async def handle(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        if not self.config.enabled:
            return {"ok": False, "reason": OFF_REASON}
        if name == "find_contact":
            return await self.find_contact(str(args.get("name") or ""))
        if name == "share_file":
            return await self.prepare(str(args.get("path") or ""), str(args.get("to") or ""),
                                      str(args.get("via") or ""), str(args.get("message") or ""))
        return {"ok": False, "reason": f"unknown tool {name}"}

    async def ask_and_share(self, args: Mapping[str, Any], ask: Ask) -> dict[str, Any]:
        """The whole share_file flow for a caller with a yes/no prompt: the question, the owner's answer, the send.
        The token never reaches the model."""
        from . import consent

        prep = await self.handle("share_file", args)
        if not prep.get("needs_approval"):
            return prep
        token = str(prep["token"])
        try:
            answer = (await ask(str(prep["question"]))).strip().lower()
        except BaseException:
            self.cancel(token)
            raise
        if not consent.approves(answer):
            self.cancel(token)
            return {"ok": False, "reason": "the owner said no; do not retry it"}
        return await self.confirm(token)

    # -- contacts --
    async def lookup(self, name: str) -> tuple[Optional[list[dict[str, Any]]], int, str]:
        """(people, how many matched, why not). People carry name, phones and emails as [{label, value}]."""
        if not self.config.imessage:
            return None, 0, "Contacts can only be read on the Mac"
        q = _CONTROL.sub("", name).strip()[:100]
        if not q:
            return None, 0, "no name given"
        rc, out, _ = await self.runner(["osascript", "-l", "JavaScript", "-", q, str(MAX_MATCHES)],
                                       CONTACTS_JXA.encode(), CONTACTS_SECS)
        log.info("share: contacts lookup rc=%d", rc)
        if rc == RC_TIMEOUT:
            return None, 0, "Contacts didn't answer in time"
        if rc != 0:
            return None, 0, ("Contacts can't be read: allow eve under System Settings > Privacy & Security > "
                             "Contacts (and Automation > Contacts)")
        try:
            body = json.loads(out.strip() or "{}")
        except ValueError:
            return None, 0, "Contacts gave an answer that couldn't be read"
        people = []
        for p in body.get("people") or []:
            people.append({"name": str(p.get("name") or "").strip(),
                           "phones": [{"label": _label(x.get("label")), "value": str(x.get("value") or "").strip()}
                                      for x in p.get("phones") or [] if x.get("value")],
                           "emails": [{"label": _label(x.get("label")), "value": str(x.get("value") or "").strip()}
                                      for x in p.get("emails") or [] if x.get("value")]})
        log.info("share: contacts matched %d", len(people))
        return people, int(body.get("count") or len(people)), ""

    async def find_contact(self, name: str) -> dict[str, Any]:
        people, count, why = await self.lookup(name)
        if people is None:
            return {"ok": False, "reason": why}
        out: dict[str, Any] = {"ok": True, "people": people}
        if count > len(people):
            out["more"] = f"{count - len(people)} more matched; use more of the name"
        return out

    async def _recipient(self, to: str, via: str) -> dict[str, Any]:
        """{"handle", "who"} or {"ok": False, ...} (with ``matches`` when the owner must pick)."""
        to = _CONTROL.sub("", to).strip()
        if not to:
            return {"ok": False, "reason": "no recipient given"}
        if is_email(to):
            return {"handle": to, "who": to}
        if is_phone(to):
            if via == "email":
                return {"ok": False, "reason": "email needs an email address, not a phone number"}
            return {"handle": phone_handle(to), "who": to}
        people, count, why = await self.lookup(to)
        if people is None:
            return {"ok": False, "reason": why}
        if not people:
            return {"ok": False, "reason": "no one by that name in the owner's Contacts"}
        if count > 1:
            return {"ok": False, "ambiguous": True, "matches": people,
                    "reason": "more than one person matches: ask the owner which, then pass their number or "
                              "address as `to`"}
        person = people[0]
        if via == "email":
            options = [e["value"] for e in person["emails"]]
        else:
            mobiles = [p["value"] for p in person["phones"] if p["label"] in ("mobile", "iphone", "cell")]
            options = mobiles if len(mobiles) == 1 else [p["value"] for p in person["phones"]] + [
                e["value"] for e in person["emails"]]
        options = list(dict.fromkeys(options))
        if not options:
            kind = "email address" if via == "email" else "phone number or email address"
            return {"ok": False, "reason": f"{person['name']} has no {kind} in Contacts"}
        if len(options) > 1:
            return {"ok": False, "ambiguous": True, "matches": [person],
                    "reason": f"{person['name']} has several; ask the owner which, then pass it as `to`"}
        handle = options[0]
        if via == "imessage" and is_phone(handle):
            handle = phone_handle(handle)
        return {"handle": handle, "who": f"{person['name']} ({options[0]})"}

    # -- the question --
    async def prepare(self, raw_path: str, to: str, via: str, note: str) -> dict[str, Any]:
        if via not in VIAS:
            return {"ok": False, "reason": "via is imessage or email"}
        if via == "imessage" and not self.config.imessage:
            return {"ok": False, "reason": "iMessage is only on the Mac"}
        if via == "email" and not self.email_on:
            return {"ok": False, "reason": "email sharing needs the Google sign-in (eve-bridge apps login "
                                           "google) with Drive allowed to write"}
        path, why = resolve_file(raw_path, self.home)
        if path is None:
            return {"ok": False, "reason": why}
        # What the owner approves is these exact bytes at this inode (review, 2026-10-05: size and mtime alone,
        # read through a link, let a swapped-in link to a hidden key go out); stage_exact checks them at the send.
        try:
            st, digest = await asyncio.to_thread(fingerprint, path)
        except (OSError, FileChanged):
            return {"ok": False, "reason": "that file can't be read"}
        cap = MAX_IMESSAGE_BYTES if via == "imessage" else MAX_DRIVE_BYTES
        attach = via == "email" and self.gmail and st.st_size <= MAX_GMAIL_ATTACH_BYTES
        if st.st_size > cap:
            return {"ok": False, "reason": f"too big to share ({human_size(st.st_size)}; the limit is "
                                           f"{human_size(cap)})"}
        rec = await self._recipient(to, via)
        if "handle" not in rec:
            return rec
        note = _CONTROL.sub("", note).strip()[:MAX_NOTE]
        self._sweep_pending()
        token = secrets.token_urlsafe(12)
        self._pending[token] = Pending(token, via, path, st.st_size, st.st_mtime_ns, rec["handle"], rec["who"],
                                       note, self.clock(), raw=raw_path, ino=st.st_ino, dev=st.st_dev, sha256=digest,
                                       attach=attach)
        how = ("by iMessage" if via == "imessage" else "by email, attached, from your Gmail" if attach
               else "by email (a Google Drive link)")
        question = f"Send {path.name} ({human_size(st.st_size)}) to {rec['who']} {how}?"
        if note:
            question += f'\nNote: "{note}"'
        log.info("share: asking (%s, %d bytes)", via, st.st_size)
        return {"ok": True, "needs_approval": True, "question": question, "token": token}

    def _sweep_pending(self) -> None:
        now = self.clock()
        for t in [t for t, p in self._pending.items() if now - p.created > APPROVAL_TTL_SECS]:
            del self._pending[t]

    def cancel(self, token: str) -> None:
        self._pending.pop(token, None)

    # -- the send --
    async def confirm(self, token: str) -> dict[str, Any]:
        """Send what the question described, once. Nothing runs for an unknown, used or expired token, or when
        the file changed since the question."""
        p = self._pending.pop(token, None)
        if p is None or self.clock() - p.created > APPROVAL_TTL_SECS:
            log.info("share: refused a token that is unknown or expired")
            return {"ok": False, "reason": "that share is no longer pending; ask again"}
        real, _why = resolve_file(p.raw, self.home)            # the owner-path rules again, at the send
        if real is None or real != p.path:
            log.info("share: refused, the path no longer leads to the file the owner was asked about")
            return {"ok": False, "reason": "the file changed since the owner was asked; nothing was sent"}
        if p.via == "imessage":
            return await self._send_imessage(p)
        if p.attach:
            return await asyncio.to_thread(self._send_gmail, p)
        return await asyncio.to_thread(self._send_drive, p)

    async def _send_imessage(self, p: Pending) -> dict[str, Any]:
        outbox = (self.outbox or OUTBOX).expanduser()
        try:
            outbox.mkdir(parents=True, exist_ok=True)
            self._sweep_outbox(outbox)
            staged = outbox / f"{secrets.token_hex(4)}-{p.path.name}"
            await asyncio.to_thread(stage_exact, p, staged)
        except FileChanged:
            log.info("share: refused, the file changed since the question")
            return {"ok": False, "reason": "the file changed since the owner was asked; nothing was sent"}
        except OSError as e:
            log.warning("share: could not stage the file (%s)", type(e).__name__)
            return {"ok": False, "reason": "couldn't copy the file for Messages; nothing was sent"}
        rc, _, err = await self.runner(["osascript", "-", p.handle, str(staged), p.note],
                                       MESSAGES_SEND.encode(), SEND_SECS)
        log.info("share: imessage rc=%d (%d bytes)", rc, p.size)
        if rc == RC_TIMEOUT:
            return {"ok": False, "reason": "Messages didn't answer in time, so it may or may not have sent; check "
                                           "Messages before trying again"}
        if rc != 0:
            low = err.lower()
            if "-1743" in low or "not authorized" in low:
                why = ("eve isn't allowed to control Messages: System Settings > Privacy & Security > "
                       "Automation, allow Messages for eve")
            else:
                why = "Messages refused it (is iMessage signed in, and is that number or address on iMessage?)"
            staged.unlink(missing_ok=True)
            return {"ok": False, "reason": why}
        return {"ok": True, "sent": p.path.name, "to": p.who, "via": "imessage", "bytes": p.size,
                "note": "Messages took it; delivery isn't confirmed by Messages"}

    def _sweep_outbox(self, outbox: Path) -> None:
        cutoff = time.time() - OUTBOX_KEEP_SECS
        for f in outbox.iterdir():
            try:
                if f.is_file() and f.stat().st_mtime < cutoff:
                    f.unlink()
            except OSError:
                pass

    # -- Gmail --
    def _send_gmail(self, p: Pending) -> dict[str, Any]:
        """The approved bytes, staged, as one email's attachment from the owner's Gmail."""
        with tempfile.TemporaryDirectory(prefix="eve-share-") as tmp:
            staged = Path(tmp) / p.path.name
            try:
                stage_exact(p, staged)
                mail = attachment_mail(p.handle, p.path.name, p.note, staged.read_bytes())
            except FileChanged:
                log.info("share: refused, the file changed since the question")
                return {"ok": False, "reason": "the file changed since the owner was asked; nothing was sent"}
            except OSError as e:
                log.warning("share: could not stage the file (%s)", type(e).__name__)
                return {"ok": False, "reason": "couldn't read the file; nothing was sent"}
            except ValueError:                         # a name no email header can carry (a control character)
                log.info("share: refused, the file's name cannot be emailed")
                return {"ok": False, "reason": "that file's name can't be emailed; rename it and ask again; "
                                               "nothing was sent"}
        try:
            sent = self._drive("POST", GMAIL_SEND_UPLOAD, params={"uploadType": "media"}, content=mail,
                               headers={"Content-Type": "message/rfc822"}).json()
        except (DriveError, httpx.HTTPError, ValueError) as e:
            log.warning("share: gmail send failed (%s)", type(e).__name__)
            detail = str(e).replace("Drive said", "Gmail said") if isinstance(e, DriveError) else type(e).__name__
            return {"ok": False, "reason": f"{detail}; nothing was sent"}
        log.info("share: gmail sent (%d bytes)", p.size)
        return {"ok": True, "sent": p.path.name, "to": p.who, "via": "email", "bytes": p.size, "attached": True,
                "id": sent.get("id") if isinstance(sent, dict) else None, "note": "sent from the owner's Gmail"}

    # -- Drive --
    def _drive(self, method: str, url: str, **kw: Any) -> httpx.Response:
        tok = self.token() if self.token is not None else None
        if not tok:
            raise DriveError("not signed in to Google (eve-bridge apps login google)")
        client = self.http or httpx.Client(timeout=DRIVE_SECS)
        try:
            r = client.request(method, url, headers={"Authorization": f"Bearer {tok}", **kw.pop("headers", {})},
                               **kw)
        finally:
            if self.http is None:
                client.close()
        if r.status_code >= 400:
            try:
                msg = r.json().get("error", {}).get("message") or r.text
            except (ValueError, AttributeError):
                msg = r.text
            raise DriveError(f"Drive said HTTP {r.status_code}: {str(msg)[:200]}")
        return r

    def _folder(self) -> str:
        if self._folder_id:
            return self._folder_id
        key, value = FOLDER_PROP
        q = (f"mimeType='{FOLDER_MIME}' and trashed=false and "
             f"appProperties has {{ key='{key}' and value='{value}' }}")
        found = self._drive("GET", f"{DRIVE}/files", params={"q": q, "fields": "files(id)", "pageSize": 1}).json()
        files = found.get("files") or []
        if files:
            self._folder_id = str(files[0]["id"])
        else:
            made = self._drive("POST", f"{DRIVE}/files", params={"fields": "id"},
                               json={"name": FOLDER_NAME, "mimeType": FOLDER_MIME, "appProperties": {key: value}})
            self._folder_id = str(made.json()["id"])
        return self._folder_id

    def _upload(self, path: Path, size: int, folder: str) -> dict[str, Any]:
        mime = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        meta = {"name": path.name, "parents": [folder]}
        if size <= MULTIPART_MAX:
            boundary = "eve-share-" + secrets.token_hex(8)
            body = (f"--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{json.dumps(meta)}\r\n"
                    f"--{boundary}\r\nContent-Type: {mime}\r\n\r\n").encode() + path.read_bytes() + \
                f"\r\n--{boundary}--".encode()
            return self._drive("POST", DRIVE_UPLOAD, params={"uploadType": "multipart", "fields": FILE_FIELDS},
                               content=body, headers={"Content-Type": f"multipart/related; boundary={boundary}"}).json()
        start = self._drive("POST", DRIVE_UPLOAD, params={"uploadType": "resumable", "fields": FILE_FIELDS},
                            json=meta, headers={"X-Upload-Content-Type": mime, "X-Upload-Content-Length": str(size)})
        session = start.headers.get("location")
        if not session:
            raise DriveError("Drive gave no upload address")
        return self._drive("PUT", session, content=_chunks(path),
                           headers={"Content-Type": mime, "Content-Length": str(size)}).json()

    def _send_drive(self, p: Pending) -> dict[str, Any]:
        # The approved bytes are copied first, to a private folder, and the upload reads that copy (stage_exact).
        with tempfile.TemporaryDirectory(prefix="eve-share-") as tmp:
            staged = Path(tmp) / p.path.name
            try:
                stage_exact(p, staged)
            except FileChanged:
                log.info("share: refused, the file changed since the question")
                return {"ok": False, "reason": "the file changed since the owner was asked; nothing was sent"}
            except OSError as e:
                log.warning("share: could not stage the file (%s)", type(e).__name__)
                return {"ok": False, "reason": "couldn't read the file; nothing was sent"}
            return self._send_drive_staged(p, staged)

    def _send_drive_staged(self, p: Pending, staged: Path) -> dict[str, Any]:
        step = "folder"
        try:
            folder = self._folder()
            step = "upload"
            meta = self._upload(staged, p.size, folder)
            step = "share"
            params = {"sendNotificationEmail": "true", "fields": "id"}
            if p.note:
                params["emailMessage"] = p.note
            self._drive("POST", f"{DRIVE}/files/{meta['id']}/permissions", params=params,
                        json={"type": "user", "role": "reader", "emailAddress": p.handle})
        except (DriveError, httpx.HTTPError, KeyError, ValueError, OSError) as e:
            log.warning("share: drive %s failed (%s)", step, type(e).__name__)
            what = {"folder": "nothing was uploaded", "upload": "nothing was uploaded",
                    "share": "the file is in the owner's Drive (eve shared) but was NOT shared"}[step]
            detail = str(e) if isinstance(e, DriveError) else type(e).__name__
            return {"ok": False, "reason": f"{detail}; {what}"}
        log.info("share: drive shared (%d bytes)", p.size)
        return {"ok": True, "sent": p.path.name, "to": p.who, "via": "email", "bytes": p.size,
                "link": meta.get("webViewLink") or "", "note": "Google emails them the link"}


class DriveError(Exception):
    pass


def attachment_mail(to: str, filename: str, note: str, data: bytes) -> bytes:
    """One RFC 2822 email: To, Subject (the file's name), the note as the body, the file attached by its type.
    From is left out: Gmail fills in the signed-in account."""
    from email.message import EmailMessage
    from email.policy import SMTP

    msg = EmailMessage(policy=SMTP)
    msg["To"] = to
    msg["Subject"] = filename
    msg.set_content(note or "")
    mime = mimetypes.guess_type(filename)[0] or "application/octet-stream"
    main, _, sub = mime.partition("/")
    msg.add_attachment(data, maintype=main, subtype=sub or "octet-stream", filename=filename)
    return msg.as_bytes()


def gmail_can_send(environ: Optional[Mapping[str, str]] = None) -> bool:
    """Whether Gmail may send: the apps policy lets it, and the stored sign-in granted gmail.send (a file check,
    no network). A sign-in from before 2026-10-09 has no gmail.send, so email shares stay Drive links until the
    owner signs in again."""
    from . import apps_mcp

    cfg = apps_mcp.configured(environ)
    if not cfg.enabled or GMAIL_SEND_SCOPE not in apps_mcp.google_scopes(cfg.policy):
        return False
    granted = str(apps_mcp._read_json(cfg.google_token_path).get("scope") or "").split()
    return GMAIL_SEND_SCOPE in granted


def _chunks(path: Path) -> Iterator[bytes]:
    with path.open("rb") as f:
        while True:
            block = f.read(CHUNK)
            if not block:
                return
            yield block


def make_share(environ: Optional[Mapping[str, str]] = None,
               token: Optional[Callable[[], Optional[str]]] = None) -> Optional[Share]:
    """The share tools, or None when off (then no tool is lent). ``token`` (apps_mcp.TokenSource("google", ...))
    turns email on; lend it only when the Google sign-in has Drive's write scope."""
    cfg = configured(environ)
    if not cfg.enabled:
        log.info("share: off (EVE_SHARE)")
        return None
    if not cfg.imessage and token is None:
        log.info("share: on, but neither iMessage (macOS) nor Google is here; off")
        return None
    gmail = token is not None and gmail_can_send(environ)
    log.info("share: on (imessage=%s, email=%s, attached=%s)", cfg.imessage, token is not None, gmail)
    return Share(cfg, token=token, gmail=gmail)


def google_token(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[], Optional[str]]]:
    """The direct apps' Google token when it can write to Drive (drive.file), else None."""
    from . import apps_mcp

    cfg = apps_mcp.configured(environ)
    if not cfg.enabled:
        return None
    if "https://www.googleapis.com/auth/drive.file" not in apps_mcp.google_scopes(cfg.policy):
        return None
    src = apps_mcp.TokenSource("google", cfg)
    return src if src.available else None

