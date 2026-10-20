"""Write python_parity.json: what the live Python (google_rest.py on CPython 3.13) answers for a set of inputs.

The Rust tests read the JSON and assert the port answers the same, so the parity claim is a measurement. Re-run
from the repo root with the bridge's interpreter, in Los Angeles time, after google_rest.py changes:

    TZ=America/Los_Angeles <bridge venv>/bin/python -I rust/crates/eve-google-rest/tests/fixtures/gen_parity.py
"""
from __future__ import annotations

import base64
import html
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))
assert os.environ.get("TZ") == "America/Los_Angeles", "run with TZ=America/Los_Angeles"

from eve_bridge import google_rest as g  # noqa: E402


def b64(text: str) -> str:
    return base64.urlsafe_b64encode(text.encode()).decode().rstrip("=")


def attempt(fn, *a):
    try:
        return {"ok": fn(*a)}
    except Exception as e:  # noqa: BLE001
        return {"err": f"{type(e).__name__}: {e}"}


out: dict = {}

ISO = ["2026-10-05", "2026-10-05T09:00", "2026-10-05T09", "2026-10-05 09:00:00", "20261005T0900",
       "2026-10-05T09:00:00.5", "2026-10-05T09:00:00,123456789", "2026-10-05T09:00:00+05:30",
       "2026-10-05T09:00:00-0700", "2026-10-05T09:00:00+00:00", "2026-10-05T09:00:00Z", "2026-W41-1",
       "2026W411T10:00", "2026-W41", "2026W41", "2026-W53-1", "2020-W53-7", "next friday", "2026-13-01",
       "2026-10-05T24:00:00", "2026-10-05T09:00:00+05", "2026-10-05T09:00:00+0530",
       "2026-10-05T09:00:00.123+01:00", "2026-10-05X09:00", "2026-10-05T9:00", "2026-10-05T09:00:00 ",
       " 2026-10-05", "2026-10-5", "2026-10-05T09:00:00+05:30:15", "2026-10-05T09:00:00-00:00", "2026-02-30",
       "0000-01-01", "2026-10-05T09:00:60", "2026-10-05T0900", "2026-10-05T09:00:00.", "2026-10-05T09:00:00+5:30",
       "2026-10-05T09:00:00.1234567", "2026-10-05T09:00:00-07:00:00", "2026-10-05T23:59:59.999999-12:00",
       "2026-1005", "202610-05", "2026-10-05T", "2026-10-05T09:00:00Z+01:00", "2026-10-05T09:00:00+0",
       "1999-12-31T23:59:59.000001+14:00", "2026-10-05T09:00-07", "2026-10-05T09:00:00.000+00:00"]
out["fromisoformat"] = {s: attempt(lambda s: datetime.fromisoformat(s).isoformat(), s) for s in ISO}
DATES = ["2026-10-05", "20261005", "2026-W41-1", "2026W411", "2026-W41", "2026-10-5", "2026-10-05T00:00", "x"]
out["date_fromisoformat"] = {s: attempt(lambda s: __import__("datetime").date.fromisoformat(s).isoformat(), s)
                             for s in DATES}

RFC = ["2026-10-05T09:00", "2026-10-05", "2026-10-05T09:00:00Z", "next friday", "  2026-03-08T02:30 ",
       "2026-11-01T01:30", "2026-10-05T09:00:00.25", "2026-W41-1", "", "2026-03-08T02:00", "2026-11-01T00:59:59",
       "2026-07-04T12:00:00+02:00", "2026-01-15T08:00"]
out["rfc3339"] = {s: g.rfc3339(s) for s in RFC}
out["local_zone"] = g._local_zone()

ENT = ["Hi &amp; bye", "&lt;p&gt;", "&notit;", "&notin;", "&#39;", "&#x27;", "&#128;", "&#0;", "&#xD800;",
       "&#1114112;", "&#11;", "&copy", "&copyx", "&amp", "&#65", "&#x41g", "&", "&;", "&#;", "&unknownentity;",
       "&ampamp;", "a &nbsp;b", "&#99999999999999999999;", "&NotNestedGreaterGreater;", "&#13;", "&#x1F600;"]
out["unescape"] = {s: html.unescape(s) for s in ENT}

MAIL = [
    {"mimeType": "multipart/alternative", "parts": [
        {"mimeType": "text/html", "body": {"data": b64("<p>html</p>")}},
        {"mimeType": "text/plain", "body": {"data": b64("plain text\n\n\nend")}}]},
    {"mimeType": "text/html", "body": {"data": b64("<style>x{}</style><p>Hi &amp; bye</p>")}},
    {"mimeType": "text/html", "body": {"data": b64(
        "<html><head><SCRIPT type='x'>var a = '<b>';</script></head><body><div>One</div>\n<div>Two &lt;3"
        "</div><br/>  &nbsp;Three\r\n Four</body></html>")}},
    {"mimeType": "text/plain", "body": {"data": b64("  lead\r\n\ttabbed\x0bvt\x1cfs\u0085nel  \n\n")}},
    {"mimeType": "text/plain", "body": {"data": b64("café " * 900)}},
    {"mimeType": "text/plain", "body": {"data": "!!!not base64***"}},
    {"mimeType": "text/plain", "body": {"data": base64.urlsafe_b64encode(b"bad \xff\xfe utf8").decode()}},
    {"mimeType": "text/html", "body": {"data": b64("<p unclosed")}},
]
out["mail_text"] = [[p, g.mail_text(p)] for p in MAIL]

PLACES = ["Soda Hall 306, Berkeley, CA", "  Room 101  ", "", None, "https://zoom.us/j/123456",
          "meet.google.com/abc-defg-hij", "abc-defg-hij", "Zoom: zoom.us/j/99", "Microsoft Teams Meeting", "TEAMS",
          "www.example.com", "Café & Bar #2", "x" * 5000, "Online", "N/A", "na", "Phone call", "zoom meeting",
          "Room  4\tB", "example.io/path", "Google Meet", "123 Main St, Apt #4/5", "ftp://x"]
out["place_links"] = [[p, g.place_links(p)] for p in PLACES]

DESC = ["short", "Agenda. " + "y" * 900 + " join https://meet.google.com/abc-defg-hij now",
        "z" * 900 + " https://meet.google.com/abc-defg-hij", "a" * 499 + "  " + "b" * 10,
        "https://zoom.us/j/1 " + "q" * 600 + " https://meet.google.com/xyz", "é" * 600 + " https://x.webex.com/m/1",
        "w" * 495 + " https://teams.microsoft.com/l/meetup-join/abc tail"]
out["event_description"] = [[d, g._event_description(d)] for d in DESC]

ADDR = [["ana@example.com"], "bo@example.com, cy@example.org", ["ana@example.com\nBcc: eve@example.com"], ["Ana"],
        [], None, "  ", ["a@b.co", " , c@d.co ,"], ["x" * 100 + "@y.com no"], ["é@x.com"], ["a@b"], 5,
        ["a@b.c@d.e"], ["<a@b.co>"], ["a@b.co;c@d.co"], ["it's@x.co"]]
out["addresses"] = [[a, attempt(g.addresses, a)] for a in ADDR]


def mail(to, cc, bcc, subject, body, reply=None):
    try:
        raw = g.build_mail(to, cc, bcc, subject, body, reply)
    except Exception as e:  # noqa: BLE001
        return {"err": f"{type(e).__name__}: {e}"}
    return base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4)).decode("latin-1")


SUBJECTS = ["Hi", "", "Café plans for the weekend",
            "A very long subject line that goes on and on and on and on well past seventy eight chars",
            "Ça va? Très bien, merci beaucoup pour tout ce que vous avez fait pour nous cette année",
            "x" * 90, "Re: " + "word " * 14 + "end", "é" * 60,
            "Hello 日本語 world 中文 and more ascii words here to push it past the line limit ok",
            "Re: Lunch?", "tab\there", "  padded  ", "a  b   c", "Price: $5 (approx) [draft] <x> @home; ok, \"q\"",
            "\U0001F600 party", "word " * 30, "Supercalifragilisticexpialidocious" * 3,
            "short é then a very long tail of plain ascii words that keeps going until it must fold over",
            "été " * 20, "mixed é and ü and ñ spaced far apart in a long line of text ok yes",
            "trailing space ", "x" * 70 + " é"]
BODIES = ["Hello\nthere", "x", "Ünïcode body ok", "word " * 30 + "\nshort line \nend\t", "é" * 100,
          "x=y " * 40, "line\r\nwith\rcr\n", "a" * 78, "a" * 79, "=" * 80, "tab\t" * 25, "é " * 50 + "\n" * 12 +
          "end", "\n".join(["long " * 20] * 12), "trailing \n" * 3 + "x" * 100 + " ", "é" * 30 + "\n" + "b" * 90,
          "From the start\n.dot line\n", "\x0cformfeed\x0bvt"]
cases = []
for s in SUBJECTS:
    cases.append([["a@b.co"], [], [], s, "b", None])
for b in BODIES:
    cases.append([["a@b.co"], [], [], "s", b, None])
cases.append([["a@b.co"], ["c@d.co", "e@f.co"], ["g@h.co"], "", "x", {"In-Reply-To": "<m@x>",
                                                                        "References": "<r@x> <m@x>"}])
cases.append([[f"person{i}@example.com" for i in range(8)], [], [], "s", "b", None])
cases.append([[f"p{i}@ex.co" for i in range(20)], [f"c{i}@ex.co" for i in range(5)], [], "s", "b", None])
cases.append([["a.very.long.local.part.that.keeps.going.and.going.and.going.on@example-domain.com",
               "b@c.co"], [], [], "s", "b", None])
cases.append([["x" * 80 + "@y.com"], [], [], "s", "b", None])
cases.append([["a@b.co"], [], [], "s", "b", {"References": " ".join(
    f"<msgid-number-{i}@mail.gmail.com>" for i in range(4)), "In-Reply-To": "<x@y>"}])
cases.append([["a@b.co"], [], [], "s", "b", {"In-Reply-To": "", "References": "<only@x>"}])
# encoded words the subject itself holds are read back by the header parser; addresses outside ASCII
for s in ["=?utf-8?q?hi?= there", "=?utf-8?q?=C3=A9t=C3=A9?= plans", "x=?utf-8?q?a?=y and more", "=?bogus?x?y?=",
          "=?utf-8?b?w6k=?= =?utf-8?b?w6k=?= end", "=?iso-8859-1?q?caf=E9?= ok", "=?utf-8?q?a b?= c",
          "=?utf-8?q?unterminated", "Re: =?UTF-8?B?w6l0w6k=?=" + " word" * 14]:
    cases.append([["a@b.co"], [], [], s, "b", None])
cases.append([["a@b.co"], [], [], "line\u2028sep", "b", None])
cases.append([["a.very.long.local.part.that.keeps.going@домен-очень-длинный.рф", "é@b.co"], [], [], "a\x0bb", "b",
              None])
for to in [["josé@x.co"], ["user@домен.рф"], ["a.very.long.local.part.that.keeps.going@домен-очень-длинный.рф", "é@b.co"],
           ["a(b)@x.co"], ["a[1]@x.co"], [f"p{i}@пример.рф" for i in range(6)]]:
    cases.append([to, [], [], "s", "b", None])
# In-Reply-To and References are msg-id lists on the bridge's CPython (3.13.14): Gmail's long ids, a list that
# folds, commas, a comment, a missing ">", text that is not a msg-id, and the parser's own failures
GMAIL_ID = "<CAFakeMessageIdForParityTests0000000000000000000000+A@mail.gmail.com>"
for refs in [GMAIL_ID, " ".join([GMAIL_ID] * 3), "<a@b> <c@d>", "<" + "m" * 60 + "@mail.gmail.com> <a@b>",
             "<a@b>,<c@d>, <e@f>", "(note) <a@b> (x y) <c@d>", "<a@b", "<a@b> junk after", "not a msg-id",
             "<a.@x.co> <.a@b.co> <a..b@c.co>", "<\"q s\"@x.co>", "<a@[1.2.3.4]> <b@[ c ]>", "<a@[a\\]b]>",
             "<a@b.(c)d>", "<a@ b> <c (x) @d>", "<a@b@c>", "<é@x.co>", "<a@[ x]>",
             "<" + "x" * 90 + "@y.com> <" + "z" * 30 + "@w.com>"]:
    cases.append([["a@b.co"], [], [], "s", "b", {"In-Reply-To": refs.split(" ")[0] or refs, "References": refs}])
out["build_mail"] = [[c, mail(*c)] for c in cases]

LA = timezone(timedelta(hours=-7))


def slots(busy, start, end, minutes, ds=9, de=18):
    parse = datetime.fromisoformat
    return attempt(lambda: g.free_slots([(parse(a), parse(b)) for a, b in busy], parse(start), parse(end), minutes,
                                        ds, de))


FREE = [
    [[["2026-10-05T10:00:00-07:00", "2026-10-05T11:00:00-07:00"],
      ["2026-10-05T11:15:00-07:00", "2026-10-05T17:00:00-07:00"]], "2026-10-05T00:00:00-07:00",
     "2026-10-06T00:00:00-07:00", 30, 9, 18],
    [[], "2026-10-30T00:00:00-07:00", "2026-11-04T00:00:00-08:00", 60, 9, 18],
    [[], "2026-10-06T00:00:00+00:00", "2026-10-07T00:00:00+00:00", 60, 9, 18],
    [[["2026-10-05T16:00:00+00:00", "2026-10-06T00:00:00+00:00"]], "2026-10-05T08:00:00-07:00",
     "2026-10-05T20:00:00-07:00", 45, 9, 18],
    [[], "2026-10-05T00:00:00-07:00", "2026-10-05T20:00:00-07:00", 30, 0, 18],
    [[], "2026-10-05T00:00:00-07:00", "2026-10-05T20:00:00-07:00", 30, 0, 30],
    [[], "2026-10-05T00:00:00-07:00", "2026-10-05T20:00:00-07:00", 30, 24, 18],
    [[], "2026-10-05T00:00:00-07:00", "2026-10-05T20:00:00-07:00", 30, -1, 18],
    [[], "2026-03-07T00:00:00-08:00", "2026-03-10T00:00:00-07:00", 60, 1, 4],
    [[], "2026-10-01T00:00:00-07:00", "2026-12-31T00:00:00-08:00", 15, 9, 18],
    [[["2026-10-05T09:30:00-07:00", "2026-10-05T10:00:00-07:00"],
      ["2026-10-05T09:00:00-07:00", "2026-10-05T09:20:00-07:00"],
      ["2026-10-05T12:00:00-04:00", "2026-10-05T12:30:00-04:00"]], "2026-10-05T09:10:00.5-07:00",
     "2026-10-05T18:00:00-07:00", 10, 9, 18],
    [[], "2026-10-05T10:00:00-07:00", "2026-10-05T09:00:00-07:00", 30, 9, 18],
    [[], "2026-10-05T00:00:00-07:00", "2026-10-06T00:00:00-07:00", -5, 9, 18],
]
out["free_slots"] = [[c, slots(*c)] for c in FREE]
out["tools"] = g.TOOLS

(HERE / "python_parity.json").write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
print("wrote", HERE / "python_parity.json")
