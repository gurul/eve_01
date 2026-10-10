"""share.py: the contacts lookup and Messages send (argv-only values), the Drive upload + reader permission, the
approval gate, the owner-path rules, ambiguous contacts and raw handles.

Nothing here sends anything: osascript is a fake runner and Drive is an httpx.MockTransport. Phone numbers are
from the 555-01xx fiction range and addresses use example.com."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
from typing import Any, Optional

import httpx
import pytest

from eve_bridge import share as S

ON = S.ShareConfig(enabled=True, imessage=True)
ANA = {"name": "Ana Lee", "phones": [{"label": "_$!<Mobile>!$_", "value": "+1 (555) 010-0199"}],
       "emails": [{"label": "_$!<Home>!$_", "value": "ana@example.com"}]}
ANA2 = {"name": "Ana Lopez", "phones": [{"label": "_$!<Mobile>!$_", "value": "+1 555 010-0123"}], "emails": []}


def run(coro: Any) -> Any:
    return asyncio.run(coro)


class FakeRunner:
    def __init__(self, people: Optional[list[dict[str, Any]]] = None, send_rc: int = 0, send_err: str = "") -> None:
        self.people = people or []
        self.send_rc = send_rc
        self.send_err = send_err
        self.calls: list[tuple[list[str], bytes]] = []

    async def __call__(self, argv: list[str], stdin: bytes, timeout: float) -> tuple[int, str, str]:
        self.calls.append((list(argv), stdin))
        if "JavaScript" in argv:
            return 0, json.dumps({"count": len(self.people), "people": self.people}) + "\n", ""
        return self.send_rc, "", self.send_err

    @property
    def sends(self) -> list[tuple[list[str], bytes]]:
        return [c for c in self.calls if "JavaScript" not in c[0]]


@pytest.fixture()
def home(tmp_path: Path) -> Path:
    h = tmp_path / "home"
    (h / "Documents").mkdir(parents=True)
    (h / "Documents" / "report.pdf").write_bytes(b"%PDF-1.4 " + b"x" * 2000)
    return h


def make(home: Path, runner: Optional[FakeRunner] = None, **kw: Any) -> S.Share:
    return S.Share(ON, runner=runner or FakeRunner([ANA]), home=home, outbox=home / "Pictures" / "eve-outbox",
                   **kw)


def share_args(to: str = "Ana Lee", via: str = "imessage", path: str = "~/Documents/report.pdf",
               message: Optional[str] = "here it is") -> dict[str, Any]:
    return {"path": path, "to": to, "via": via, "message": message}


# ---- contacts lookup ----------------------------------------------------------------------------------------

def test_contacts_lookup_passes_the_name_only_through_argv(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    name = 'Ana" & do shell script "rm -rf ~'
    out = run(sh.handle("find_contact", {"name": name}))
    assert out["ok"] and out["people"][0]["name"] == "Ana Lee"
    argv, script = r.calls[0]
    assert argv[:4] == ["osascript", "-l", "JavaScript", "-"]
    assert argv[4] == name
    assert name.encode() not in script and b"function run(argv)" in script
    # Contacts' built-in labels are cleaned
    assert out["people"][0]["phones"][0]["label"] == "mobile"


def test_ambiguous_contact_returns_both_and_asks_nothing(home: Path) -> None:
    r = FakeRunner([ANA, ANA2])
    sh = make(home, r)
    out = run(sh.handle("share_file", share_args(to="Ana")))
    assert out["ok"] is False and out["ambiguous"] is True
    assert [p["name"] for p in out["matches"]] == ["Ana Lee", "Ana Lopez"]
    assert "needs_approval" not in out and not sh._pending and not r.sends


def test_one_person_with_several_handles_for_email_is_ambiguous(home: Path) -> None:
    two = dict(ANA, emails=[{"label": "home", "value": "ana@example.com"},
                            {"label": "work", "value": "ana.lee@example.org"}])
    sh = make(home, FakeRunner([two]), token=lambda: "tok")
    out = run(sh.handle("share_file", share_args(via="email")))
    assert out["ambiguous"] is True and not sh._pending


def test_no_match(home: Path) -> None:
    out = run(make(home, FakeRunner([])).handle("share_file", share_args(to="Nobody")))
    assert out["ok"] is False and "Contacts" in out["reason"]


@pytest.mark.parametrize("to,handle", [("+1 (555) 010-0199", "+15550100199"), ("ana@example.com", "ana@example.com")])
def test_raw_phone_or_email_skips_the_lookup(home: Path, to: str, handle: str) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    out = run(sh.handle("share_file", share_args(to=to)))
    assert out["needs_approval"] is True
    assert not any("JavaScript" in c[0] for c in r.calls)
    assert sh._pending[out["token"]].handle == handle


def test_phone_is_refused_for_email(home: Path) -> None:
    out = run(make(home, token=lambda: "t").handle("share_file", share_args(to="+15550100199", via="email")))
    assert out["ok"] is False and "email address" in out["reason"]


# ---- approval gate + Messages send --------------------------------------------------------------------------

def test_share_asks_first_and_sends_nothing(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    out = run(sh.handle("share_file", share_args()))
    assert out["needs_approval"] is True and out["token"]
    assert out["question"].startswith("Send report.pdf (2 KB) to Ana Lee (+1 (555) 010-0199) by iMessage?")
    assert 'Note: "here it is"' in out["question"]
    assert not r.sends


def test_confirm_sends_with_values_only_in_argv(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    out = run(sh.handle("share_file", share_args(message='hi" & quit')))
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is True and done["via"] == "imessage"
    [(argv, script)] = r.sends
    assert argv[0:2] == ["osascript", "-"]
    handle, staged, note = argv[2:]
    assert handle == "+15550100199" and note == 'hi" & quit'
    staged_path = Path(staged)
    assert staged_path.parent == home / "Pictures" / "eve-outbox" and staged_path.name.endswith("report.pdf")
    assert staged_path.read_bytes() == (home / "Documents" / "report.pdf").read_bytes()
    for value in (handle, staged, note):
        assert value.encode() not in script
    assert b"on run argv" in script and b"service type = iMessage" in script and b"POSIX file" in script


def test_token_is_single_use_and_unknown_tokens_run_nothing(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    out = run(sh.handle("share_file", share_args()))
    assert run(sh.confirm(out["token"]))["ok"]
    again = run(sh.confirm(out["token"]))
    assert again["ok"] is False
    assert run(sh.confirm("made-up"))["ok"] is False
    assert len(r.sends) == 1


def test_expired_token_runs_nothing(home: Path) -> None:
    now = [1000.0]
    r = FakeRunner([ANA])
    sh = make(home, r, clock=lambda: now[0])
    out = run(sh.handle("share_file", share_args()))
    now[0] += S.APPROVAL_TTL_SECS + 1
    assert run(sh.confirm(out["token"]))["ok"] is False and not r.sends


def test_stale_file_is_refused(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    out = run(sh.handle("share_file", share_args()))
    f = home / "Documents" / "report.pdf"
    f.write_bytes(b"changed")
    os.utime(f, ns=(1, 1))
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and "changed" in done["reason"] and not r.sends


def _same_stamp(f: Path, like: os.stat_result) -> None:
    os.utime(f, ns=(like.st_atime_ns, like.st_mtime_ns), follow_symlinks=False)


def test_a_file_swapped_for_a_symlink_after_the_question_is_not_sent(home: Path) -> None:
    """Review, 2026-10-05: confirm checked size and mtime through stat(), which follows a link, so a report.pdf
    swapped for a link to ~/.ssh's key of the same size and mtime sent the key. The hidden-path rule ran only
    when the question was asked."""
    r = FakeRunner([ANA])
    sh = make(home, r)
    f = home / "Documents" / "report.pdf"
    out = run(sh.handle("share_file", share_args()))
    before = f.stat()
    (home / ".ssh").mkdir()
    key = home / ".ssh" / "id_ed25519"
    key.write_bytes(b"-----BEGIN OPENSSH PRIVATE KEY-----".ljust(before.st_size, b"A"))
    _same_stamp(key, before)
    f.unlink()
    f.symlink_to(key)
    assert f.stat().st_size == before.st_size and f.stat().st_mtime_ns == before.st_mtime_ns
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and not r.sends
    outbox = home / "Pictures" / "eve-outbox"
    assert not outbox.exists() or not any(b"PRIVATE KEY" in p.read_bytes() for p in outbox.iterdir())


def test_a_file_whose_bytes_changed_with_the_same_size_and_mtime_is_not_sent(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    f = home / "Documents" / "report.pdf"
    out = run(sh.handle("share_file", share_args()))
    before = f.stat()
    with f.open("r+b") as fh:                                   # same inode, same size, new bytes
        fh.write(b"SECRET!!")
    _same_stamp(f, before)
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and "changed" in done["reason"] and not r.sends


def test_a_file_replaced_by_another_file_of_the_same_size_and_mtime_is_not_sent_by_email(home: Path) -> None:
    drive = FakeDrive(folder_exists=True)
    sh = make(home, FakeRunner([ANA]), token=lambda: "tok", http=httpx.Client(transport=httpx.MockTransport(drive)))
    f = home / "Documents" / "report.pdf"
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email")))
    before = f.stat()
    other = home / "Documents" / "other.pdf"
    other.write_bytes(b"x" * before.st_size)
    _same_stamp(other, before)
    os.replace(other, f)                                        # a new inode at the same path
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and not drive.requests


def test_ask_and_share_denied_sends_nothing(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)
    asked: list[str] = []

    async def ask(q: str) -> str:
        asked.append(q)
        return "no"

    out = run(sh.ask_and_share(share_args(), ask))
    assert out["ok"] is False and asked and not r.sends and not sh._pending


def test_ask_and_share_allowed_sends_once(home: Path) -> None:
    r = FakeRunner([ANA])
    sh = make(home, r)

    async def ask(q: str) -> str:
        return "yes"

    out = run(sh.ask_and_share(share_args(), ask))
    assert out["ok"] is True and "token" not in out and len(r.sends) == 1


def test_messages_not_authorized_is_explained(home: Path) -> None:
    sh = make(home, FakeRunner([ANA], send_rc=1, send_err="execution error: Not authorized (-1743)"))
    out = run(sh.handle("share_file", share_args()))
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and "Automation" in done["reason"]


# ---- path rules ---------------------------------------------------------------------------------------------

def test_path_rules(home: Path, tmp_path: Path) -> None:
    (home / ".ssh").mkdir()
    (home / ".ssh" / "id").write_text("k")
    outside = tmp_path / "outside.txt"
    outside.write_text("x")
    r = FakeRunner([ANA])
    sh = make(home, r)
    for path, words in ((str(outside), "home"), ("~/.ssh/id", "hidden"), ("~/Documents", "folder"),
                        ("~/Documents/missing.pdf", "no such file")):
        out = run(sh.handle("share_file", share_args(path=path)))
        assert out["ok"] is False and words in out["reason"], (path, out)
    assert not r.calls and not sh._pending


def test_off_lends_nothing() -> None:
    assert S.make_share({"EVE_SHARE": "0"}) is None
    assert S.configured({}).enabled is False
    assert S.configured({"EVE_SHARE": "1"}, platform="linux").imessage is False


def test_tools_offer_only_the_live_ways(home: Path) -> None:
    sh = make(home)
    assert sh.tools()[0]["parameters"]["properties"]["via"]["enum"] == ["imessage"]
    assert make(home, token=lambda: "t").tools()[0]["parameters"]["properties"]["via"]["enum"] == ["imessage", "email"]
    assert {t["name"] for t in sh.tools()} == set(S.TOOL_NAMES)


# ---- Drive ---------------------------------------------------------------------------------------------------

class FakeDrive:
    def __init__(self, folder_exists: bool = False) -> None:
        self.requests: list[httpx.Request] = []
        self.folder_exists = folder_exists

    def __call__(self, req: httpx.Request) -> httpx.Response:
        self.requests.append(req)
        url = str(req.url)
        assert req.headers["authorization"] == "Bearer tok"
        if req.method == "GET" and url.startswith(S.DRIVE + "/files"):
            return httpx.Response(200, json={"files": [{"id": "fold1"}] if self.folder_exists else []})
        if req.method == "POST" and url.startswith(S.DRIVE + "/files?"):
            return httpx.Response(200, json={"id": "fold1"})
        if req.method == "POST" and url.startswith(S.DRIVE_UPLOAD) and "uploadType=multipart" in url:
            return httpx.Response(200, json={"id": "file1", "name": "report.pdf",
                                             "webViewLink": "https://drive.google.com/file/d/file1/view"})
        if req.method == "POST" and url.startswith(S.DRIVE_UPLOAD) and "uploadType=resumable" in url:
            return httpx.Response(200, headers={"Location": "https://upload.example.test/session1"})
        if req.method == "PUT":
            return httpx.Response(200, json={"id": "file2", "webViewLink": "https://drive.google.com/file/d/file2"})
        if "/permissions" in url:
            return httpx.Response(200, json={"id": "perm1"})
        return httpx.Response(404, json={"error": {"message": "unexpected"}})


def test_email_uploads_then_shares_as_reader_with_notification(home: Path) -> None:
    drive = FakeDrive()
    sh = make(home, FakeRunner([ANA]), token=lambda: "tok", http=httpx.Client(transport=httpx.MockTransport(drive)))
    out = run(sh.handle("share_file", share_args(via="email")))
    assert "by email" in out["question"] and not drive.requests
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is True and done["link"].startswith("https://drive.google.com/")
    methods = [(r.method, r.url.path) for r in drive.requests]
    assert methods[0] == ("GET", "/drive/v3/files")                      # find eve's folder
    assert methods[1] == ("POST", "/drive/v3/files")                     # make it
    assert json.loads(drive.requests[1].content)["mimeType"] == S.FOLDER_MIME
    upload = drive.requests[2]
    assert upload.url.params["uploadType"] == "multipart"
    assert b'"parents": ["fold1"]' in upload.content and b"%PDF-1.4" in upload.content
    perm = drive.requests[3]
    assert perm.url.path == "/drive/v3/files/file1/permissions"
    assert perm.url.params["sendNotificationEmail"] == "true"
    assert perm.url.params["emailMessage"] == "here it is"
    assert json.loads(perm.content) == {"type": "user", "role": "reader", "emailAddress": "ana@example.com"}


def test_email_large_file_goes_resumable(home: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(S, "MULTIPART_MAX", 100)
    drive = FakeDrive(folder_exists=True)
    sh = make(home, FakeRunner([ANA]), token=lambda: "tok", http=httpx.Client(transport=httpx.MockTransport(drive)))
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email", message=None)))
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is True
    kinds = [r.method for r in drive.requests]
    assert kinds == ["GET", "POST", "PUT", "POST"]
    put = drive.requests[2]
    assert str(put.url) == "https://upload.example.test/session1"
    assert put.headers["content-length"] == str((home / "Documents" / "report.pdf").stat().st_size)
    assert "emailMessage" not in drive.requests[3].url.params


def test_email_share_failure_says_the_file_was_not_shared(home: Path) -> None:
    def drive(req: httpx.Request) -> httpx.Response:
        if "/permissions" in str(req.url):
            return httpx.Response(403, json={"error": {"message": "forbidden"}})
        return FakeDrive(folder_exists=True)(req)

    sh = make(home, FakeRunner([ANA]), token=lambda: "tok", http=httpx.Client(transport=httpx.MockTransport(drive)))
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email")))
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and "NOT shared" in done["reason"]


def test_email_off_without_google(home: Path) -> None:
    out = run(make(home).handle("share_file", share_args(via="email")))
    assert out["ok"] is False and "Google" in out["reason"]


# ---- email as a Gmail attachment (owner, 2026-10-09) --------------------------------------------------------------

class FakeGmail:
    def __init__(self, status: int = 200) -> None:
        self.status = status
        self.requests: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        return httpx.Response(self.status, json={"id": "m1"} if self.status == 200 else {"error": {"message": "no"}})


def _gmail(home: Path, gmail: Optional[FakeGmail] = None) -> tuple[S.Share, FakeGmail]:
    g = gmail or FakeGmail()
    return make(home, FakeRunner([ANA]), token=lambda: "tok", gmail=True,
                http=httpx.Client(transport=httpx.MockTransport(g))), g


def test_an_email_share_is_a_gmail_attachment_of_the_approved_bytes(home: Path) -> None:
    import email
    from email.policy import default

    sh, g = _gmail(home)
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email", message="the report")))
    assert "by email, attached, from your Gmail" in out["question"] and g.requests == []      # asked, not sent
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is True and done["attached"] is True and len(g.requests) == 1
    r = g.requests[0]
    assert str(r.url).startswith(S.GMAIL_SEND_UPLOAD) and r.url.params["uploadType"] == "media"
    assert r.headers["content-type"] == "message/rfc822" and r.headers["authorization"] == "Bearer tok"
    msg = email.message_from_bytes(r.content, policy=default)
    assert msg["To"] == "ana@example.com" and msg["Subject"] == "report.pdf"
    part = next(msg.iter_attachments())
    assert part.get_filename() == "report.pdf" and part.get_content_type() == "application/pdf"
    assert part.get_content() == (home / "Documents" / "report.pdf").read_bytes()          # the exact bytes
    assert "the report" in msg.get_body(("plain",)).get_content()


def test_a_file_changed_after_the_question_is_not_attached(home: Path) -> None:
    sh, g = _gmail(home)
    f = home / "Documents" / "report.pdf"
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email")))
    before = f.stat()
    with f.open("r+b") as fh:
        fh.write(b"SECRET!!")
    _same_stamp(f, before)
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and "changed" in done["reason"] and g.requests == []


def test_a_big_file_or_no_gmail_send_goes_as_a_drive_link(home: Path, monkeypatch: Any) -> None:
    monkeypatch.setattr(S, "MAX_GMAIL_ATTACH_BYTES", 100)                     # report.pdf is ~2 KB
    sh, _g = _gmail(home)
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email")))
    assert "a Google Drive link" in out["question"] and sh._pending[out["token"]].attach is False
    monkeypatch.undo()
    plain = make(home, FakeRunner([ANA]), token=lambda: "tok")                 # gmail off: a link, as before
    out = run(plain.handle("share_file", share_args(to="ana@example.com", via="email")))
    assert "a Google Drive link" in out["question"]
    assert "Google Drive link" in plain.tools()[0]["description"] and "Gmail" not in plain.instructions()
    sh2, _ = _gmail(home)
    assert "attached, from the owner's Gmail" in sh2.tools()[0]["description"]
    assert "attachment from the owner's Gmail" in sh2.instructions()


def test_a_gmail_refusal_says_nothing_was_sent(home: Path) -> None:
    sh, g = _gmail(home, FakeGmail(status=403))
    out = run(sh.handle("share_file", share_args(to="ana@example.com", via="email")))
    done = run(sh.confirm(out["token"]))
    assert done["ok"] is False and "Gmail said HTTP 403" in done["reason"] and "nothing was sent" in done["reason"]


def test_gmail_can_send_needs_the_policy_and_the_granted_scope(tmp_path: Path) -> None:
    def env(scope: str, **extra: str) -> dict[str, str]:
        (tmp_path / "google-token.json").write_text(json.dumps({"refresh_token": "r", "scope": scope}))
        return {"EVE_APPS": "1", "EVE_APPS_DIR": str(tmp_path), **extra}

    g = "https://www.googleapis.com/auth/"
    assert S.gmail_can_send(env(f"{g}gmail.readonly {g}gmail.send")) is True
    assert S.gmail_can_send(env(f"{g}gmail.readonly")) is False                    # an older sign-in
    assert S.gmail_can_send(env(f"{g}gmail.send", EVE_APPS_POLICY="gmail=read")) is False
    assert S.gmail_can_send(env(f"{g}gmail.send", EVE_APPS="0")) is False
