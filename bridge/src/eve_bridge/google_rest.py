"""Gmail, Calendar and Drive through Google's plain REST APIs, shaped like their MCP servers.

Google's Workspace MCP servers answer only for a Cloud project enrolled in the Workspace Developer Preview
Program. Live 2026-10-04 with the owner's personal gmail: "Access to this tool requires that your Google Cloud
project (...) is enrolled in the Google Workspace Developer Preview Program", and a personal account does not
get in. The same sign-in reads the plain APIs, so eve uses those. The tool names and arguments follow the MCP
servers' own (gmail search_threads, calendar list_events, suggest_time, respond_to_event, drive search_files,
download_file_content...), so the brain and Meet's calendar lookup read the same either way.

Every call is grounded in Google's reference (checked 2026-10-04):

* Gmail v1 users.threads / users.messages / users.messages.attachments / users.labels to read (gmail.readonly).
  Bodies: the first text/plain part that is not an attachment, else text/html stripped. users.messages.send to
  send (gmail.send, which can send and nothing else): a base64url RFC 2822 message in ``raw``; a reply carries
  the thread's threadId and In-Reply-To/References set to the original's Message-ID, or Gmail starts a new
  thread. Every send is the owner's yes first (apps_mcp's "ask" policy), shown whole.
* People v1 people:searchContacts and otherContacts:search (contacts.readonly, contacts.other.readonly): the
  owner's saved contacts, and the people they have emailed who were never saved. Google asks for one search
  with an empty query first, to warm its cache, else results lag; the client does that once.
* Calendar v3 events.list / get / insert / patch / delete / quickAdd / instances, freeBusy.query,
  calendarList.list. timeMin/timeMax must carry an offset (RFC 3339), so a bare local time gets the Mac's.
  A PATCH merges nested objects, so switching all-day and timed sends the other key as null. sendUpdates
  decides whether guests are emailed (the API's default is no email). Every event handed back is compacted
  (``compact_event``) and carries Google Maps links for its place (``place_links``), so "my next class and a
  maps link" is one calendar call.
* Drive v3 files.list / get / export, alt=media downloads. Google Docs export as Markdown (headings and
  tables kept), Sheets as CSV, Slides as text; exports are capped at 10 MB by Google. PDFs are read with
  macOS's PDFKit (pyobjc Quartz, already a dependency), so no PDF library is added.

The client duck-types apps_mcp.McpClient: ``list_tools()`` and ``call_tool()``, results in MCP's shape
(``structuredContent``, ``isError``). Lists carry ``nextPageToken`` and take ``pageToken``. Mail bodies and
file text are clipped; nothing is logged but types.
"""

from __future__ import annotations

import base64
import html as html_mod
import json
import logging
import re
import secrets
from datetime import date, datetime, time, timedelta, timezone
from typing import Any, Callable, Mapping, Optional
from urllib.parse import quote

import httpx

log = logging.getLogger(__name__)

GMAIL = "https://gmail.googleapis.com/gmail/v1/users/me"
PEOPLE = "https://people.googleapis.com/v1"
CALENDAR = "https://www.googleapis.com/calendar/v3"
DRIVE = "https://www.googleapis.com/drive/v3"
DRIVE_UPLOAD = "https://www.googleapis.com/upload/drive/v3/files"
BODY_CLIP = 4000                 # characters of one mail body
FILE_CLIP = 50_000               # characters of one file's or attachment's text
THREADS_MAX = 10                 # threads summarised per search (each is one more request)
DOWNLOAD_MAX = 25 * 1024 * 1024  # bytes read from one file or attachment
SEARCH_BACK_DAYS = 365           # search_events looks this far back (and forward without a limit)
CALENDARS_MAX = 20               # list_events with no calendarId reads this many calendars at most, one request each
EVENT_DESC_CLIP = 500            # characters of one event's description handed to the model
PLACE_CLIP = 500                 # characters of a location put in a Maps link (Google's URL limit is 2,048)
RECIPIENTS_MAX = 20              # addresses in one email's To, Cc and Bcc together
CONTACTS_MAX = 10                # people per contacts search (Google's own cap is 30)


def _tool(name: str, description: str, props: Mapping[str, Any], required: tuple[str, ...] = (),
          reads: bool = True) -> dict[str, Any]:
    return {"name": name, "description": description,
            "inputSchema": {"type": "object", "properties": dict(props), "required": list(required)},
            "annotations": {"readOnlyHint": reads}}


_S = {"type": "string"}
_B = {"type": "boolean"}
_PAGE = {"type": "integer", "description": "How many to return (default 10)."}
_TOKEN = {"type": "string", "description": "nextPageToken from the previous call, for the next page."}
_WHEN = {"type": "string", "description": "ISO 8601, e.g. 2026-10-10T12:00:00-07:00 (no offset: the owner's own)."}
_NOTIFY = {"type": "boolean", "description": "Email the guests about it (default: yes when there are guests)."}

TOOLS: dict[str, list[dict[str, Any]]] = {
    "gmail": [
        _tool("search_threads", "Search mail threads with Gmail search syntax (from:, to:, is:unread, "
              "newer_than:2d, has:attachment, subject:...). Returns each thread's subject, sender, date and snippet.",
              {"query": _S, "pageSize": _PAGE, "pageToken": _TOKEN}),
        _tool("get_thread", "Read a whole thread: every message's sender, date, subject, text and attachments.",
              {"threadId": _S}, ("threadId",)),
        _tool("get_message", "Read one message: sender, date, subject, text and attachments.",
              {"messageId": _S}, ("messageId",)),
        _tool("get_attachment", "Read an attachment's text (PDF and text files) by the attachmentId that "
              "get_message or get_thread listed.", {"messageId": _S, "attachmentId": _S, "filename": _S},
              ("messageId", "attachmentId")),
        _tool("list_labels", "The mailbox's labels, with the inbox's total and unread counts.", {}),
        _tool("send_message", "Send an email from the owner's Gmail. to, cc and bcc are email addresses; \"me\" "
              "is the owner's own. To answer "
              "a message, pass its messageId as replyToMessageId: the email goes into that thread, and the subject "
              "defaults to the original's with Re:. body is plain text.",
              {"to": {"type": "array", "items": _S}, "cc": {"type": "array", "items": _S},
               "bcc": {"type": "array", "items": _S}, "subject": _S, "body": _S, "replyToMessageId": _S},
              ("to", "body"), reads=False),
    ],
    "contacts": [
        _tool("search_contacts", "Find people by name, email or phone in the owner's Google contacts, and among "
              "the people they have emailed. Returns names, email addresses, phone numbers and organizations.",
              {"query": _S, "pageSize": _PAGE}, ("query",)),
    ],
    "calendar": [
        _tool("list_calendars", "The owner's calendars (id, name, primary).", {"pageSize": _PAGE}),
        _tool("list_events", "Events between startTime and endTime, soonest first, recurring events as single "
              "occurrences, each tagged with its calendar. Default: every calendar the owner sees except "
              "holidays (a calendarId reads that one only), from now, 7 days.",
              {"calendarId": _S, "startTime": _WHEN, "endTime": _WHEN, "fullText": _S, "pageSize": _PAGE,
               "pageToken": _TOKEN}),
        _tool("search_events", "Find events on the primary calendar by words in the title, description, place "
              "or attendees, from a year ago on, soonest first.", {"query": _S, "pageSize": _PAGE, "pageToken": _TOKEN},
              ("query",)),
        _tool("get_event", "One event in full.", {"calendarId": _S, "eventId": _S}, ("eventId",)),
        _tool("suggest_time", "Free slots of at least durationMinutes between startTime and endTime, busy time taken "
              "from every calendar the owner sees except holidays, within working hours (default 9 to 18).",
              {"startTime": _WHEN, "endTime": _WHEN, "durationMinutes": {"type": "integer"},
               "workdayStartHour": {"type": "integer"}, "workdayEndHour": {"type": "integer"}},
              ("startTime", "endTime", "durationMinutes")),
        _tool("create_event", "Create an event. With allDay, startTime and endTime are dates (YYYY-MM-DD, end "
              "exclusive). recurrence takes RRULE lines, e.g. [\"RRULE:FREQ=WEEKLY;BYDAY=MO\"].",
              {"summary": _S, "startTime": _WHEN, "endTime": _WHEN, "allDay": _B, "timeZone": _S,
               "description": _S, "location": _S, "attendeeEmails": {"type": "array", "items": _S},
               "recurrence": {"type": "array", "items": _S}, "addGoogleMeetUrl": _B, "calendarId": _S,
               "notifyGuests": _NOTIFY}, ("summary", "startTime", "endTime"), reads=False),
        _tool("quick_add_event", "Create an event from one plain sentence, the way Google Calendar's quick add "
              "reads it: \"Lunch with Ana Friday 12pm at Zuni\".", {"text": _S, "calendarId": _S}, ("text",),
              reads=False),
        _tool("update_event", "Change an event: only the fields given change. For a recurring event's one "
              "occurrence, use that occurrence's eventId (list_events returns them).",
              {"eventId": _S, "calendarId": _S, "summary": _S, "startTime": _WHEN, "endTime": _WHEN, "allDay": _B,
               "timeZone": _S, "description": _S, "location": _S,
               "addAttendeeEmails": {"type": "array", "items": _S}, "notifyGuests": _NOTIFY},
              ("eventId",), reads=False),
        _tool("respond_to_event", "Answer an invitation as the owner: accepted, declined or tentative.",
              {"eventId": _S, "calendarId": _S, "responseStatus": {"type": "string", "enum": [
                  "accepted", "declined", "tentative"]}, "notifyGuests": _NOTIFY},
              ("eventId", "responseStatus"), reads=False),
        _tool("delete_event", "Delete an event.", {"eventId": _S, "calendarId": _S, "notifyGuests": _NOTIFY},
              ("eventId",), reads=False),
    ],
    "drive": [
        _tool("search_files", "Find files by words in their name or text (or a Drive query such as "
              "\"name contains 'budget'\"). Returns id, name, type, modified time and link.",
              {"query": _S, "pageSize": _PAGE, "pageToken": _TOKEN}, ("query",)),
        _tool("list_recent_files", "The most recently changed files.", {"pageSize": _PAGE, "pageToken": _TOKEN}),
        _tool("get_file_metadata", "One file's name, type, owners, size, modified time and link.",
              {"fileId": _S}, ("fileId",)),
        _tool("read_file_content", "A file's text: Docs as Markdown, Sheets as CSV (first sheet), Slides as text, "
              "PDFs and plain text files as text.", {"fileId": _S}, ("fileId",)),
        _tool("create_file", "Create a file in the owner's Drive from text. mimeType "
              "application/vnd.google-apps.document makes it a Google Doc.",
              {"title": _S, "textContent": _S, "mimeType": _S}, ("title",), reads=False),
    ],
}
# read_file_content is also the MCP server's download_file_content for what it can read.
FILE_FIELDS = "id,name,mimeType,modifiedTime,webViewLink,owners(displayName),size"
EXPORTS = {"application/vnd.google-apps.document": "text/markdown",
           "application/vnd.google-apps.spreadsheet": "text/csv",
           "application/vnd.google-apps.presentation": "text/plain"}
TEXT_TYPES = ("application/json", "application/xml", "application/csv")
_ADDRESS = re.compile(r"[^@\s<>,;\"]+@[^@\s<>,;\"]+\.[^@\s<>,;\"]+")
CONTACT_FIELDS = "names,emailAddresses,phoneNumbers,organizations"
OTHER_FIELDS = "names,emailAddresses,phoneNumbers"


class GoogleError(Exception):
    pass


# -- pure helpers --
def _header(msg: Mapping[str, Any], name: str) -> str:
    for h in (msg.get("payload") or {}).get("headers") or []:
        if str(h.get("name", "")).lower() == name.lower():
            return str(h.get("value") or "")
    return ""


def _decode(data: str) -> bytes:
    try:
        return base64.urlsafe_b64decode(data + "=" * (-len(data) % 4))
    except (ValueError, TypeError):
        return b""


def _walk(part: Mapping[str, Any]):
    yield part
    for child in part.get("parts") or []:
        yield from _walk(child)


def mail_text(part: Mapping[str, Any]) -> str:
    """A message's readable text: the first text/plain part that is not an attachment, else text/html with the
    tags dropped (a .txt attachment is never taken for the body)."""
    plain, html = [], []
    for p in _walk(part):
        data = (p.get("body") or {}).get("data")
        if not data or p.get("filename"):
            continue
        mime = str(p.get("mimeType") or "")
        if mime == "text/plain":
            plain.append(_decode(data).decode("utf-8", "replace"))
        elif mime == "text/html":
            html.append(_decode(data).decode("utf-8", "replace"))
    if plain:
        text = plain[0]
    elif html:
        text = re.sub(r"<(script|style)[^>]*>.*?</\1>|<[^>]+>", " ", html[0], flags=re.S | re.I)
        text = html_mod.unescape(text)
    else:
        text = ""
    text = "\n".join(line.strip() for line in text.splitlines() if line.strip())
    return text if len(text) <= BODY_CLIP else text[:BODY_CLIP] + "…"


def attachments(part: Mapping[str, Any]) -> list[dict[str, Any]]:
    return [{"filename": p["filename"], "mimeType": p.get("mimeType"), "size": (p.get("body") or {}).get("size"),
             "attachmentId": (p.get("body") or {}).get("attachmentId")}
            for p in _walk(part) if p.get("filename") and (p.get("body") or {}).get("attachmentId")]


def _message(msg: Mapping[str, Any], full: bool) -> dict[str, Any]:
    out = {"id": msg.get("id"), "threadId": msg.get("threadId"), "from": _header(msg, "From"),
           "to": _header(msg, "To"), "date": _header(msg, "Date"), "subject": _header(msg, "Subject"),
           "labels": msg.get("labelIds") or [], "snippet": msg.get("snippet", "")}
    if full:
        out["text"] = mail_text(msg.get("payload") or {})
        out["attachments"] = attachments(msg.get("payload") or {})
    return out


def rfc3339(value: Any) -> str:
    """A time Google accepts in timeMin/timeMax: with an offset. A bare date is its local midnight; a time with
    no offset is the Mac's local time. Anything unreadable is passed as it is, for Google to refuse."""
    text = str(value or "").strip()
    try:
        if re.fullmatch(r"\d{4}-\d{2}-\d{2}", text):
            dt = datetime.combine(date.fromisoformat(text), datetime.min.time())
        else:
            dt = datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError:
        return text
    if dt.tzinfo is None:
        dt = dt.astimezone()                       # the Mac's zone, which is the owner's
    return dt.isoformat()


def _when(value: Optional[str], all_day: bool, tz: Optional[str]) -> Optional[dict[str, Any]]:
    """An event's start or end. Timed: dateTime (with tz when given, else an offset is added) and date null, so a
    PATCH from all-day clears the date; all-day: the reverse."""
    if not value:
        return None
    if all_day:
        return {"date": str(value)[:10], "dateTime": None}
    out: dict[str, Any] = {"dateTime": value if tz else rfc3339(value), "date": None}
    if tz:
        out["timeZone"] = tz
    return out


def _send_updates(args: Mapping[str, Any], has_guests: bool) -> str:
    notify = args.get("notifyGuests")
    return "all" if (has_guests if notify is None else bool(notify)) else "none"


def free_slots(busy: list[tuple[datetime, datetime]], start: datetime, end: datetime, minutes: int,
               day_start: int = 9, day_end: int = 18) -> list[dict[str, str]]:
    """The free stretches of at least ``minutes`` between start and end, inside working hours each day.

    The days and their working hours are the owner's: wall-clock hours in the Mac's zone, resolved per day
    (Lean CalendarEdits P5, 2026-10-05). Built in start's fixed offset, a request in UTC offered the owner's
    night, and a span across the end of daylight time offered the hour before their day."""
    busy = sorted(busy)
    out: list[dict[str, str]] = []
    day = start.astimezone().date()

    def local(d: date, hour: int) -> datetime:
        return datetime.combine(d, time(hour)).astimezone()     # a naive time is read in the Mac's zone

    while local(day, 0) < end and len(out) < 50:
        lo = max(start, local(day, day_start))
        hi = min(end, local(day, min(day_end, 23)))
        cursor = lo
        for b0, b1 in busy:
            if b1 <= cursor or b0 >= hi:
                continue
            if (b0 - cursor) >= timedelta(minutes=minutes):
                out.append({"start": cursor.isoformat(), "end": b0.isoformat()})
            cursor = max(cursor, b1)
        if cursor < hi and (hi - cursor) >= timedelta(minutes=minutes):
            out.append({"start": cursor.isoformat(), "end": hi.isoformat()})
        day += timedelta(days=1)
    return out


def _date_only(calendar_id: str) -> bool:
    """A calendar of dates, not commitments: Google's holiday, birthday and week-number calendars."""
    return calendar_id.endswith("group.v.calendar.google.com") or "#holiday@" in calendar_id or \
        "#contacts@" in calendar_id or "#weeknum@" in calendar_id


# -- events: compact, with Maps links --
# Google Maps URLs (https://developers.google.com/maps/documentation/urls/get-started, checked 2026-10-04):
# "https://www.google.com/maps/search/?api=1&query=..." opens a place by its name or address, and
# "https://www.google.com/maps/dir/?api=1&destination=..." opens directions to it, the origin defaulting to
# where the phone is. api=1 is required; the value must be URL-encoded (spaces as %20) and a URL stays
# under 2,048 characters. No key is needed, so the link is built here and costs no Maps call. Owner's goal,
# 2026-10-04: "look for my next class on calendar and give me maps link" should be one fast step.
MAPS_SEARCH = "https://www.google.com/maps/search/?api=1&query="
MAPS_DIR = "https://www.google.com/maps/dir/?api=1&destination="
_URLISH = re.compile(r"[a-z][a-z0-9+.-]*://|\bwww\.|\b[\w-]+\.(?:com|us|net|org|io|me|co)/\S", re.I)
# A location that names a call, not a place: Outlook and Zoom fill these in on their own, and a bare Meet
# code (abc-defg-hij) is a call too.
_NOT_A_PLACE = re.compile(r"(?:microsoft\s+)?teams(?:\s+meeting)?|zoom(?:\s+meeting)?|google\s+meet|webex|"
                          r"online|virtual|remote|phone(?:\s+call)?|tbd|tba|n/?a|[a-z]{3}-[a-z]{4}-[a-z]{3}", re.I)
# A call's link past the clip is kept: meet.event_link reads the description for one.
_CALL_LINK = re.compile(r"https?://(?:meet\.google\.com|[\w.-]*zoom\.us|teams\.microsoft\.com|"
                        r"teams\.live\.com|[\w.-]*webex\.com)/\S+", re.I)
ATTENDEE_KEYS = ("email", "responseStatus", "self", "organizer")


def is_place(location: Any) -> bool:
    """A location Maps can find: not empty, not a link (a Zoom or Meet URL), not a call's name."""
    text = " ".join(str(location or "").split())
    return bool(text) and not _URLISH.search(text) and not _NOT_A_PLACE.fullmatch(text)


def place_links(location: Any) -> dict[str, str]:
    """mapsUrl (the place) and directionsUrl (how to get there) for a place, else nothing."""
    if not is_place(location):
        return {}
    q = quote(" ".join(str(location).split())[:PLACE_CLIP], safe="")
    return {"mapsUrl": MAPS_SEARCH + q, "directionsUrl": MAPS_DIR + q}


def _event_description(text: str) -> str:
    if len(text) <= EVENT_DESC_CLIP:
        return text
    clipped = text[:EVENT_DESC_CLIP].rstrip() + "…"
    link = _CALL_LINK.search(text, EVENT_DESC_CLIP)
    return clipped + " " + link.group(0) if link and not _CALL_LINK.search(clipped) else clipped


def compact_event(event: Any) -> Any:
    """An event as the model needs it: when, what, where (with its Maps links), how to join, who. Google's
    full resource (etag, creator, reminders, iCalUID, every guest's name...) is several times larger. meet.py
    reads start, summary, hangoutLink, conferenceData.entryPoints, location and description, all kept."""
    if not isinstance(event, dict):
        return event
    out: dict[str, Any] = {k: event[k] for k in ("id", "status", "summary", "start", "end", "location")
                           if event.get(k) not in (None, "")}
    out.update(place_links(event.get("location")))
    if event.get("description"):
        out["description"] = _event_description(str(event["description"]))
    for k in ("hangoutLink", "conferenceData"):
        if event.get(k):
            out[k] = event[k]
    if isinstance(event.get("attendees"), list):
        out["attendees"] = [{k: g[k] for k in ATTENDEE_KEYS if k in g} for g in event["attendees"]
                            if isinstance(g, dict)]
    if isinstance(event.get("organizer"), dict) and event["organizer"].get("email"):
        out["organizer"] = {"email": event["organizer"]["email"]}
    for k in ("recurringEventId", "htmlLink"):
        if event.get(k):
            out[k] = event[k]
    return out


def _start_key(event: Mapping[str, Any], tz_name: Any = None) -> datetime:
    """When an event starts, as an aware datetime for sorting: an all-day date is midnight in the calendar's own
    time zone (the Mac's when it has none); one that cannot be read sorts last."""
    start = event.get("start") if isinstance(event.get("start"), dict) else {}
    try:
        if start.get("dateTime"):
            when = datetime.fromisoformat(str(start["dateTime"]).replace("Z", "+00:00"))
            return when if when.tzinfo else when.astimezone()
        if start.get("date"):
            day = date.fromisoformat(str(start["date"]))
            try:
                from zoneinfo import ZoneInfo

                zone: Any = ZoneInfo(str(tz_name)) if tz_name else None
            except Exception:  # noqa: BLE001 — an unknown zone name is the Mac's own
                zone = None
            midnight = datetime(day.year, day.month, day.day)
            return midnight.replace(tzinfo=zone) if zone else midnight.astimezone()
    except ValueError:
        pass
    return datetime.max.replace(tzinfo=timezone.utc)


def _events(items: Any) -> list[Any]:
    return [compact_event(e) for e in items or []]


def pdf_text(data: bytes) -> Optional[str]:
    """A PDF's text through macOS's PDFKit (pyobjc Quartz); None where that is not available."""
    try:
        from Foundation import NSData  # type: ignore[import-not-found]
        from Quartz import PDFDocument  # type: ignore[import-not-found]
    except ImportError:
        return None
    doc = PDFDocument.alloc().initWithData_(NSData.dataWithBytes_length_(data, len(data)))
    if doc is None:
        return None
    return str(doc.string() or "")


def as_text(data: bytes, mime: str) -> Optional[str]:
    """Bytes as readable text by their type, or None when they are not text eve can read."""
    if mime == "application/pdf":
        return pdf_text(data)
    if mime.startswith("text/") or mime in TEXT_TYPES:
        return data.decode("utf-8", "replace")
    return None


def _clip_text(text: str) -> tuple[str, bool]:
    return (text[:FILE_CLIP], len(text) > FILE_CLIP)


def addresses(value: Any) -> list[str]:
    """The email addresses in a To, Cc or Bcc argument (a list, or one comma-separated string), each checked
    whole: a value that is not exactly an address (a name, a newline that would start another header) is refused,
    never trimmed into one."""
    items = [value] if isinstance(value, str) else list(value or []) if isinstance(value, (list, tuple)) else []
    out: list[str] = []
    for item in items:
        for piece in str(item).split(","):
            piece = piece.strip()
            if not piece:
                continue
            if not _ADDRESS.fullmatch(piece):
                raise ValueError(f"not an email address: {piece[:80]!r}")
            out.append(piece)
    return out


def _one_line(value: Any) -> str:
    text = str(value or "")
    if "\r" in text or "\n" in text:
        raise ValueError("a subject is one line")
    return text.strip()


def build_mail(to: list[str], cc: list[str], bcc: list[str], subject: str, body: str,
               reply_headers: Optional[Mapping[str, str]] = None) -> str:
    """The base64url RFC 2822 message Gmail's send takes as ``raw``. From is left out: Gmail fills in the
    signed-in account."""
    from email.message import EmailMessage
    from email.policy import SMTP

    msg = EmailMessage(policy=SMTP)
    msg["To"] = ", ".join(to)
    if cc:
        msg["Cc"] = ", ".join(cc)
    if bcc:
        msg["Bcc"] = ", ".join(bcc)
    msg["Subject"] = subject
    for k, v in (reply_headers or {}).items():
        if v:
            msg[k] = v
    msg.set_content(body)
    return base64.urlsafe_b64encode(msg.as_bytes()).decode().rstrip("=")


def _person(p: Mapping[str, Any]) -> dict[str, Any]:
    def values(key: str, field_name: str) -> list[str]:
        return [str(x.get(field_name)) for x in p.get(key) or [] if isinstance(x, dict) and x.get(field_name)]

    out: dict[str, Any] = {"name": next(iter(values("names", "displayName")), "")}
    for key, field_name, label in (("emailAddresses", "value", "emails"), ("phoneNumbers", "value", "phones"),
                                   ("organizations", "name", "organizations")):
        got = values(key, field_name)
        if got:
            out[label] = got
    return out


def _http_error(r: httpx.Response) -> "GoogleError":
    try:
        msg = r.json().get("error", {}).get("message") or r.text
    except (ValueError, AttributeError):
        msg = r.text
    return GoogleError(f"HTTP {r.status_code}: {str(msg)[:200]}")


# -- the client --
class GoogleRestClient:
    """One Google app (gmail | calendar | drive | contacts) over its plain API, with apps_mcp.McpClient's two
    methods."""

    def __init__(self, app: str, token: Callable[[], Optional[str]], timeout: float = 60.0,
                 http: Optional[httpx.Client] = None) -> None:
        if app not in TOOLS:
            raise ValueError(app)
        self.app = app
        self._token = token
        self._http = http or httpx.Client(timeout=timeout)
        self._contacts_warm = False

    def list_tools(self) -> list[dict[str, Any]]:
        return [dict(t) for t in TOOLS[self.app]]

    def _req(self, method: str, url: str, raw: bool = False, **kw: Any) -> Any:
        token = self._token()
        if not token:
            raise GoogleError("not signed in to Google (apps login google)")
        headers = {"Authorization": f"Bearer {token}", **kw.pop("headers", {})}
        if raw:
            return self._download(method, url, headers, **kw)
        r = self._http.request(method, url, headers=headers, **kw)
        if r.status_code >= 400:
            raise _http_error(r)
        if not r.content:
            return {}
        return r.json() if "json" in r.headers.get("content-type", "") else r.text

    def _download(self, method: str, url: str, headers: dict[str, str], **kw: Any) -> bytes:
        """A file's bytes, read as they arrive and stopped at DOWNLOAD_MAX: reading the whole body first and
        measuring it after held a file of any size in memory."""
        with self._http.stream(method, url, headers=headers, **kw) as r:
            if r.status_code >= 400:
                r.read()
                raise _http_error(r)
            data = bytearray()
            for chunk in r.iter_bytes():
                data += chunk
                if len(data) > DOWNLOAD_MAX:
                    raise GoogleError(f"the file is larger than {DOWNLOAD_MAX // (1024 * 1024)} MB")
            return bytes(data)

    def call_tool(self, name: str, arguments: Mapping[str, Any]) -> dict[str, Any]:
        handler = getattr(self, f"_{self.app}_{name}", None)
        if handler is None:
            return {"isError": True, "content": [{"type": "text", "text": f"no tool {name}"}]}
        try:
            return {"isError": False, "structuredContent": handler(dict(arguments or {}))}
        except (GoogleError, KeyError, ValueError) as e:
            text = str(e) if isinstance(e, GoogleError) else f"{type(e).__name__}: {e}"
            return {"isError": True, "content": [{"type": "text", "text": text}]}

    @staticmethod
    def _page(a: Mapping[str, Any], default: int = 10, top: int = 250) -> dict[str, Any]:
        out: dict[str, Any] = {"maxResults": max(1, min(int(a.get("pageSize") or default), top))}
        if a.get("pageToken"):
            out["pageToken"] = a["pageToken"]
        return out

    # -- gmail --
    def _gmail_search_threads(self, a: dict[str, Any]) -> dict[str, Any]:
        params = self._page(a, THREADS_MAX, THREADS_MAX)
        listing = self._req("GET", f"{GMAIL}/threads", params={"q": a.get("query") or "", **params})
        threads = []
        for t in listing.get("threads") or []:
            full = self._req("GET", f"{GMAIL}/threads/{t['id']}", params={
                "format": "metadata", "metadataHeaders": ["From", "Subject", "Date"]})
            msgs = full.get("messages") or [{}]
            first, last = msgs[0], msgs[-1]
            threads.append({"threadId": t["id"], "subject": _header(first, "Subject"), "from": _header(last, "From"),
                            "date": _header(last, "Date"), "messages": len(msgs),
                            "unread": any("UNREAD" in (m.get("labelIds") or []) for m in msgs),
                            "snippet": last.get("snippet", "")})
        return {"threads": threads, "nextPageToken": listing.get("nextPageToken")}

    def _gmail_get_thread(self, a: dict[str, Any]) -> dict[str, Any]:
        full = self._req("GET", f"{GMAIL}/threads/{a['threadId']}", params={"format": "full"})
        return {"threadId": a["threadId"], "messages": [_message(m, True) for m in full.get("messages") or []]}

    def _gmail_get_message(self, a: dict[str, Any]) -> dict[str, Any]:
        return _message(self._req("GET", f"{GMAIL}/messages/{a['messageId']}", params={"format": "full"}), True)

    def _gmail_get_attachment(self, a: dict[str, Any]) -> dict[str, Any]:
        msg = self._req("GET", f"{GMAIL}/messages/{a['messageId']}", params={"format": "full"})
        meta = next((x for x in attachments(msg.get("payload") or {}) if x["attachmentId"] == a["attachmentId"]),
                    {"filename": a.get("filename") or "", "mimeType": ""})
        body = self._req("GET", f"{GMAIL}/messages/{a['messageId']}/attachments/{a['attachmentId']}")
        data = _decode(str(body.get("data") or ""))
        mime = str(meta.get("mimeType") or "")
        if mime == "application/octet-stream" and str(meta.get("filename", "")).lower().endswith(".pdf"):
            mime = "application/pdf"
        text = as_text(data, mime)
        if text is None:
            return {"filename": meta.get("filename"), "mimeType": mime, "text": "",
                    "note": "this kind of attachment is not read as text here"}
        text, clipped = _clip_text(text)
        return {"filename": meta.get("filename"), "mimeType": mime, "text": text, "clipped": clipped}

    def _gmail_list_labels(self, a: dict[str, Any]) -> dict[str, Any]:
        labels = self._req("GET", f"{GMAIL}/labels").get("labels") or []
        inbox = self._req("GET", f"{GMAIL}/labels/INBOX")
        return {"inbox": {"total": inbox.get("threadsTotal"), "unread": inbox.get("threadsUnread")},
                "labels": [{"id": x.get("id"), "name": x.get("name"), "type": x.get("type")} for x in labels]}

    def _mine(self, value: Any) -> Any:
        """A recipient list with each "me" made the owner's own address (Gmail's profile), asked only when used."""
        items = [value] if isinstance(value, str) else list(value) if isinstance(value, (list, tuple)) else value
        if not isinstance(items, list):
            return value
        pieces = [p.strip() for item in items for p in str(item).split(",")]
        if not any(p.lower() == "me" for p in pieces):
            return value
        own = str(self._req("GET", f"{GMAIL}/profile").get("emailAddress") or "")
        return [own if p.lower() == "me" else p for p in pieces if p]

    def _gmail_send_message(self, a: dict[str, Any]) -> dict[str, Any]:
        to, cc, bcc = (addresses(self._mine(a.get(k))) for k in ("to", "cc", "bcc"))
        if not to:
            raise ValueError("no recipient: to needs at least one email address")
        if len(to) + len(cc) + len(bcc) > RECIPIENTS_MAX:
            raise ValueError(f"more than {RECIPIENTS_MAX} recipients")
        body = str(a.get("body") or "")
        if not body.strip():
            raise ValueError("the email has no body")
        subject = _one_line(a.get("subject"))
        send: dict[str, Any] = {}
        reply: dict[str, str] = {}
        if a.get("replyToMessageId"):
            orig = self._req("GET", f"{GMAIL}/messages/{a['replyToMessageId']}", params={
                "format": "metadata", "metadataHeaders": ["Message-ID", "References", "Subject"]})
            mid = _header(orig, "Message-ID")
            reply = {"In-Reply-To": mid, "References": " ".join(x for x in (_header(orig, "References"), mid) if x)}
            if orig.get("threadId"):
                send["threadId"] = orig["threadId"]
            if not subject:
                was = _header(orig, "Subject")
                subject = was if was.lower().startswith("re:") else f"Re: {was}".strip()
        send["raw"] = build_mail(to, cc, bcc, subject, body, reply)
        sent = self._req("POST", f"{GMAIL}/messages/send", json=send)
        return {"sent": True, "id": sent.get("id"), "threadId": sent.get("threadId"), "to": to,
                "subject": subject}

    # -- contacts --
    def _contacts_search_contacts(self, a: dict[str, Any]) -> dict[str, Any]:
        query = str(a.get("query") or "").strip()
        if not query:
            raise ValueError("query is empty")
        size = max(1, min(int(a.get("pageSize") or CONTACTS_MAX), 30))
        if not self._contacts_warm:
            for url, mask in ((f"{PEOPLE}/people:searchContacts", CONTACT_FIELDS),
                              (f"{PEOPLE}/otherContacts:search", OTHER_FIELDS)):
                try:
                    self._req("GET", url, params={"query": "", "readMask": mask})
                except GoogleError:
                    pass                               # the real search below says what is wrong
            self._contacts_warm = True
        saved = self._req("GET", f"{PEOPLE}/people:searchContacts",
                          params={"query": query, "readMask": CONTACT_FIELDS, "pageSize": size})
        people = [_person(r.get("person") or {}) for r in saved.get("results") or []]
        try:
            other = self._req("GET", f"{PEOPLE}/otherContacts:search",
                              params={"query": query, "readMask": OTHER_FIELDS, "pageSize": size})
        except GoogleError:
            other = {}                                 # saved contacts still answer without the other scope
        seen = {e.lower() for p in people for e in p.get("emails", [])}
        for r in other.get("results") or []:
            p = _person(r.get("person") or {})
            if not any(e.lower() in seen for e in p.get("emails", [])):
                p["saved"] = False
                people.append(p)
        return {"people": people[:size]}

    # -- calendar --
    @staticmethod
    def _cal(a: Mapping[str, Any]) -> str:
        return f"{CALENDAR}/calendars/{quote(str(a.get('calendarId') or 'primary'), safe='')}/events"

    def _calendar_list_calendars(self, a: dict[str, Any]) -> dict[str, Any]:
        items = self._req("GET", f"{CALENDAR}/users/me/calendarList", params=self._page(a, 50)).get("items") or []
        return {"calendars": [{"id": c.get("id"), "summary": c.get("summary"), "primary": bool(c.get("primary")),
                               "accessRole": c.get("accessRole")} for c in items]}

    def _calendar_list_events(self, a: dict[str, Any]) -> dict[str, Any]:
        """One calendar when calendarId is given; else every calendar the owner sees but the date-only ones,
        merged soonest first (review, 2026-10-05: primary alone missed a subscribed class calendar, and reading
        each calendar was a model round each, so a rundown with five calendars ran out of rounds). A calendar
        that fails is named in ``unreadable``; the others' events still come back."""
        if a.get("calendarId"):
            return self._list_one_calendar(a)
        try:
            cals = [c for c in self._calendar_list_calendars({"pageSize": 50})["calendars"]
                    if c.get("id") and not _date_only(str(c["id"]))]
        except (GoogleError, httpx.HTTPError):    # a dropped list still reads the primary (Lean CalendarEdits P4)
            cals = []
        if not cals:
            return self._list_one_calendar(a)
        cals.sort(key=lambda c: not c.get("primary"))     # the primary first, so a cap never drops it
        events: list[tuple[datetime, Any]] = []
        unreadable: list[str] = []
        more, tz, failure = len(cals) > CALENDARS_MAX, None, None
        for cal in cals[:CALENDARS_MAX]:
            name = str(cal.get("summary") or cal["id"])
            try:
                got = self._list_one_calendar({**a, "calendarId": cal["id"], "pageToken": None})
            except (GoogleError, httpx.HTTPError) as e:
                unreadable.append(name)
                failure = e
                continue
            tz = tz or got.get("timeZone")
            more = more or bool(got.get("nextPageToken"))
            for ev in got["events"]:
                if isinstance(ev, dict):
                    events.append((_start_key(ev, got.get("timeZone")), {**ev, "calendar": name}))
        if failure is not None and len(unreadable) == len(cals[:CALENDARS_MAX]):
            raise failure
        events.sort(key=lambda pair: pair[0])
        page = max(1, min(int(a.get("pageSize") or 50), 250))
        out: dict[str, Any] = {"timeZone": tz, "events": [ev for _, ev in events[:page]],
                               "calendarsRead": len(cals[:CALENDARS_MAX]) - len(unreadable)}
        if more or len(events) > page:
            out["more"] = "there are more events: narrow startTime/endTime, or read one calendarId"
        if unreadable:
            out["unreadable"] = unreadable
        return out

    def _list_one_calendar(self, a: dict[str, Any]) -> dict[str, Any]:
        start = rfc3339(a.get("startTime")) if a.get("startTime") else datetime.now(timezone.utc).isoformat()
        try:
            end = rfc3339(a["endTime"]) if a.get("endTime") else (
                datetime.fromisoformat(start.replace("Z", "+00:00")) + timedelta(days=7)).isoformat()
        except ValueError:
            end = None
        params: dict[str, Any] = {"timeMin": start, "singleEvents": True, "orderBy": "startTime",
                                  **self._page(a, 50)}
        if end:
            params["timeMax"] = end
        if a.get("fullText"):
            params["q"] = a["fullText"]
        body = self._req("GET", self._cal(a), params=params)
        return {"timeZone": body.get("timeZone"), "events": _events(body.get("items")),
                "nextPageToken": body.get("nextPageToken")}

    def _calendar_search_events(self, a: dict[str, Any]) -> dict[str, Any]:
        since = (datetime.now(timezone.utc) - timedelta(days=SEARCH_BACK_DAYS)).isoformat()
        body = self._req("GET", self._cal({}), params={"q": a.get("query") or "", "singleEvents": True,
                                                       "orderBy": "startTime", "timeMin": since, **self._page(a, 25)})
        return {"events": _events(body.get("items")), "nextPageToken": body.get("nextPageToken")}

    def _calendar_get_event(self, a: dict[str, Any]) -> dict[str, Any]:
        return compact_event(self._req("GET", f"{self._cal(a)}/{quote(str(a['eventId']), safe='')}"))

    def _calendar_suggest_time(self, a: dict[str, Any]) -> dict[str, Any]:
        start = datetime.fromisoformat(rfc3339(a["startTime"]))
        end = datetime.fromisoformat(rfc3339(a["endTime"]))
        cals = self._calendar_list_calendars({"pageSize": 50})["calendars"]
        # Every calendar the owner sees counts as busy time (a subscribed class or team calendar is time they
        # are not free), except the ones that are only dates: holidays, birthdays, week numbers.
        ids = [c["id"] for c in cals if not _date_only(str(c.get("id") or ""))] or ["primary"]
        body = self._req("POST", f"{CALENDAR}/freeBusy", json={
            "timeMin": start.isoformat(), "timeMax": end.isoformat(), "items": [{"id": i} for i in ids[:50]]})
        busy = [(datetime.fromisoformat(b["start"].replace("Z", "+00:00")).astimezone(start.tzinfo),
                 datetime.fromisoformat(b["end"].replace("Z", "+00:00")).astimezone(start.tzinfo))
                for cal in (body.get("calendars") or {}).values() for b in cal.get("busy") or []]
        # `is None`, not `or`: an explicit 0 is midnight, not "unset"
        day_start, day_end = a.get("workdayStartHour"), a.get("workdayEndHour")
        slots = free_slots(busy, start, end, int(a["durationMinutes"]), 9 if day_start is None else int(day_start),
                           18 if day_end is None else int(day_end))
        return {"free": slots, "calendarsChecked": len(ids)}

    def _event_body(self, a: Mapping[str, Any]) -> dict[str, Any]:
        all_day, tz = bool(a.get("allDay")), a.get("timeZone")
        body: dict[str, Any] = {k: a[k] for k in ("summary", "description", "location") if a.get(k) is not None}
        for key, field in (("startTime", "start"), ("endTime", "end")):
            when = _when(a.get(key), all_day, tz)
            if when:
                body[field] = when
        if a.get("attendeeEmails"):
            body["attendees"] = [{"email": e} for e in a["attendeeEmails"]]
        if a.get("recurrence"):
            body["recurrence"] = list(a["recurrence"])
            for field in ("start", "end"):              # a recurring event needs its zone (events.insert)
                if field in body and "dateTime" in body[field] and body[field]["dateTime"] and not tz:
                    body[field]["timeZone"] = _local_zone()
        return body

    def _calendar_create_event(self, a: dict[str, Any]) -> dict[str, Any]:
        body = self._event_body(a)
        for field in ("start", "end"):                  # nothing to clear on a new event
            body.get(field, {}).pop("date" if body.get(field, {}).get("dateTime") else "dateTime", None)
        params: dict[str, Any] = {"sendUpdates": _send_updates(a, bool(a.get("attendeeEmails")))}
        if a.get("addGoogleMeetUrl"):
            body["conferenceData"] = {"createRequest": {"requestId": secrets.token_hex(8),
                                                        "conferenceSolutionKey": {"type": "hangoutsMeet"}}}
            params["conferenceDataVersion"] = 1
        return compact_event(self._req("POST", self._cal(a), json=body, params=params))

    def _calendar_quick_add_event(self, a: dict[str, Any]) -> dict[str, Any]:
        return compact_event(self._req("POST", f"{self._cal(a)}/quickAdd",
                                       params={"text": a["text"], "sendUpdates": "none"}))

    def _calendar_update_event(self, a: dict[str, Any]) -> dict[str, Any]:
        url = f"{self._cal(a)}/{quote(str(a['eventId']), safe='')}"
        body = self._event_body({k: v for k, v in a.items() if k != "attendeeEmails"})
        guests: list[dict[str, Any]] = []
        if a.get("addAttendeeEmails") or a.get("notifyGuests") is None:
            guests = self._req("GET", url).get("attendees") or []
        if a.get("addAttendeeEmails"):
            known = {g.get("email") for g in guests}
            body["attendees"] = guests + [{"email": e} for e in a["addAttendeeEmails"] if e not in known]
        return compact_event(self._req("PATCH", url, json=body, params={
            "sendUpdates": _send_updates(a, bool(guests or body.get("attendees")))}))

    def _calendar_respond_to_event(self, a: dict[str, Any]) -> dict[str, Any]:
        url = f"{self._cal(a)}/{quote(str(a['eventId']), safe='')}"
        guests = self._req("GET", url).get("attendees") or []
        me = [g for g in guests if g.get("self")]
        if not me:
            raise GoogleError("the owner is not a guest of this event")
        for g in me:
            g["responseStatus"] = a["responseStatus"]
        # attendees is an array, so a PATCH replaces it whole: every guest is sent back as it was
        return compact_event(self._req("PATCH", url, json={"attendees": guests},
                                       params={"sendUpdates": _send_updates(a, True)}))

    def _calendar_delete_event(self, a: dict[str, Any]) -> dict[str, Any]:
        url = f"{self._cal(a)}/{quote(str(a['eventId']), safe='')}"
        guests = (self._req("GET", url).get("attendees") or []) if a.get("notifyGuests") is None else []
        self._req("DELETE", url, params={"sendUpdates": _send_updates(a, bool(guests))})
        return {"deleted": a["eventId"]}

    # -- drive --
    def _files(self, params: dict[str, Any], a: Mapping[str, Any]) -> dict[str, Any]:
        page = self._page(a)
        params.update(pageSize=page.pop("maxResults"), fields=f"nextPageToken,files({FILE_FIELDS})", **page)
        body = self._req("GET", f"{DRIVE}/files", params=params)
        return {"files": body.get("files") or [], "nextPageToken": body.get("nextPageToken")}

    def _drive_search_files(self, a: dict[str, Any]) -> dict[str, Any]:
        q = str(a.get("query") or "").strip()
        if not any(op in q for op in (" contains ", " = ", " in parents", "mimeType", "modifiedTime")):
            q = "fullText contains '" + q.replace("\\", "\\\\").replace("'", "\\'") + "'"
        return self._files({"q": q + " and trashed = false"}, a)

    def _drive_list_recent_files(self, a: dict[str, Any]) -> dict[str, Any]:
        return self._files({"orderBy": "modifiedTime desc", "q": "trashed = false"}, a)

    def _drive_get_file_metadata(self, a: dict[str, Any]) -> dict[str, Any]:
        return self._req("GET", f"{DRIVE}/files/{a['fileId']}", params={"fields": FILE_FIELDS})

    def _drive_read_file_content(self, a: dict[str, Any]) -> dict[str, Any]:
        meta = self._drive_get_file_metadata(a)
        mime = str(meta.get("mimeType") or "")
        text: Optional[str]
        if mime in EXPORTS:
            text = self._req("GET", f"{DRIVE}/files/{a['fileId']}/export", raw=True,
                             params={"mimeType": EXPORTS[mime]}).decode("utf-8", "replace")
        elif mime == "application/pdf" or mime.startswith("text/") or mime in TEXT_TYPES:
            text = as_text(self._req("GET", f"{DRIVE}/files/{a['fileId']}", raw=True, params={"alt": "media"}), mime)
        else:
            text = None
        if text is None:
            return {"name": meta.get("name"), "mimeType": mime, "text": "", "link": meta.get("webViewLink"),
                    "note": "this kind of file is not read as text here; open its link"}
        text, clipped = _clip_text(text)
        return {"name": meta.get("name"), "mimeType": mime, "text": text, "clipped": clipped}

    def _drive_create_file(self, a: dict[str, Any]) -> dict[str, Any]:
        meta = {"name": a.get("title") or "Untitled"}
        if a.get("mimeType"):
            meta["mimeType"] = a["mimeType"]
        boundary = "eve-upload-" + secrets.token_hex(8)
        body = (f"--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{json.dumps(meta)}\r\n"
                f"--{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{a.get('textContent') or ''}\r\n"
                f"--{boundary}--").encode()
        return self._req("POST", DRIVE_UPLOAD, params={"uploadType": "multipart", "fields": FILE_FIELDS},
                         content=body, headers={"Content-Type": f"multipart/related; boundary={boundary}"})


def _local_zone() -> str:
    """The Mac's IANA zone name (America/Los_Angeles), which a recurring event needs; UTC when unknown."""
    try:
        import os

        link = os.path.realpath("/etc/localtime")
        if "zoneinfo/" in link:
            return link.split("zoneinfo/", 1)[1]
    except OSError:
        pass
    return "UTC"
