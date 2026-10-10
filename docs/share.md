# Sharing a file with someone

eve can send one of your files to another person in seconds, without driving an app on the screen. Before
this, "send the report to Ana" was a computer task in which Codex worked Messages or Mail on the screen, and it
took minutes. Now it is two command-line calls (iMessage) or three HTTP calls (email).

Code: `bridge/src/eve_bridge/share.py`. Tests: `bridge/tests/test_share.py`.

## What it does

The chat's brain gets two tools:

| Tool | What it does | Approval |
|---|---|---|
| `find_contact` {name} | Finds the people in your Contacts whose name contains the words, with their phone numbers and email addresses. | None (read only) |
| `share_file` {path, to, via, message} | Sends a file in your home folder to a contact name, a phone number or an email address, `via` `imessage` or `email`, with an optional short note. | Your yes, every time |

- **iMessage**: eve finds the person in Contacts (a phone number or email address skips the lookup), then
  tells Messages to send the note and the file. Messages reports that it accepted the message. It does not
  report delivery, and eve's answer says this.
- **Email**: eve uploads the file to your Google Drive, into a folder called **eve shared** that eve
  creates, and shares it with the person as a viewer. Google then emails them the link from your account, with
  your note. eve gives you the link too.

If two people match the name, or one person has several numbers, eve gives the choices to the brain. The
brain asks you which one and does not ask for approval until the recipient is clear.

The same rules apply as for `send_file`: the file must be in your home folder, no hidden folders (`~/.ssh`,
`~/.config`, …), no folders, and the file must exist. iMessage files can be up to 100 MB. Drive files can be up
to 1 GB (eve's own limit).

## Consent

Nothing is sent on the brain's word alone.

1. `share_file` checks the file and the recipient, then returns a question, for example *"Send report.pdf
   (2.1 MB) to Ana Lee (+1 555-0100) by iMessage?"*, and a one-time token. Nothing has run at this point.
2. eve shows you the question with **Allow / Deny**. The brain never sees the token.
3. Only after **Allow** does eve send. When it asks, it records the file's real path, inode, size,
   modification time and SHA-256. Before it sends, it applies the path rules again, opens the file without
   following a link, and copies it while it checks all of these. It sends that copy, never the path opened a
   second time. If anything differs (the file was edited, replaced, or swapped for a link), eve sends nothing
   and says why.
4. The question is asked only while no other question waits for you. Otherwise your typed yes for the older
   question could approve this one, so eve asks you to answer the older one first.
5. A token is used once and expires after 10 minutes. A denied, unknown or expired token runs nothing.

The exact phone number or address you saw in the question is the one used.

## Switching it on

`EVE_SHARE=1` in eve's env file, then restart the daemon. The default is off.

- iMessage needs macOS, with Messages signed in to iMessage.
- Email needs the direct apps' Google sign-in (`EVE_APPS=1`, `eve-bridge apps login google`) with
  Drive allowed to write (the default Drive policy, `ask`, includes the `drive.file` scope). `drive.file` only
  lets eve see and share files that eve created. It cannot share your other Drive files.

## macOS permissions to grant once

The daemon runs as a Python process under launchd. The first share asks macOS for two permissions. If you
miss the prompts, eve's answer tells you where to go:

1. **Contacts**: System Settings → Privacy & Security → **Contacts** → turn on eve's Python (the
   `.venv/bin/python` of the bridge).
2. **Automation**: System Settings → Privacy & Security → **Automation** → under eve's Python (or
   `osascript`), turn on **Messages** and **Contacts**.

To trigger the prompts on purpose, run one `find_contact` from the chat (Contacts only, sends nothing). The
Messages prompt appears on the first real share.

## Why the file is copied to ~/Pictures/eve-outbox

Messages is sandboxed. On macOS 15 and later, when a script sends a file from a folder such as `~/Documents`
or `~/Downloads`, Messages shows "Not Delivered" while the script reports success. Text still goes through. See
the [MacScripter thread on Sequoia](https://www.macscripter.net/t/sequoia-messages-send-file/76468) and
[this report on the Messages sandbox](https://github.com/anthropics/claude-plugins-official/issues/1113).
`~/Pictures` works. `~/Library/Messages` also works, but other processes can only write there with Full Disk
Access. So eve copies the file to `~/Pictures/eve-outbox` and sends the copy. Copies older than an hour
are deleted on the next share.

## Email: attached from your Gmail, or a Drive link

Since 2026-10-09 (owner: "the chat path should be able to send files"), an email share is a **real attachment**
from your own Gmail whenever Gmail may send: the apps policy for Gmail is not `read`, and your Google sign-in
granted `gmail.send` (`share.gmail_can_send`, a file check). The question says "by email, attached, from your
Gmail". The bytes you approved are staged exactly as for iMessage (the same inode, size, mtime and SHA-256 check)
and built into one email: To, the file's name as the Subject, your note as the body, the file attached by its
type. It is posted to Gmail's media upload send (`message/rfc822`). Gmail delivers 25 MB at most and encoding adds
a third, so a file over 18 MB goes as a Drive link instead, and the question says so.

A sign-in from before 2026-10-09 has no `gmail.send`, so email shares stay Drive links until you run
`eve-bridge apps login google` again.

### The Drive link

Sharing was built while Gmail was read only, when eve had no permission to send mail. Instead, the Drive API's
[permissions.create](https://developers.google.com/drive/api/reference/rest/v3/permissions/create) with
`sendNotificationEmail=true` has Google send the email: it comes from your account, it carries your note
(`emailMessage`), and the recipient gets a link to a file only they can view. eve never needs a mail-sending
permission for a file, and you can remove the person's access later in Drive.

## Safety

- Every value (name, number, path, note) goes to `osascript` as an argument, and the script is read on stdin.
  No value is ever part of a script's text, so a note or a name cannot inject AppleScript. No shell is used.
- Logs show only sizes, counts and exit codes. They never show file names, recipients or notes.
- On a timeout, eve says the message may or may not have gone and does not retry.
