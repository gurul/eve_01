"""Write python_parity.json: what the live Python (apps_mcp.py and github_repos.py) answers for a set of inputs.

The Rust test (tests/test_parity.rs) reads the JSON and asserts the port answers the same, so the parity claim is
a measurement. Re-run from the repo root with the bridge's interpreter after either module changes:

    <bridge venv>/bin/python -I rust/crates/eve-apps-mcp/tests/fixtures/gen_parity.py
"""
from __future__ import annotations

import json
import random
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))

from eve_bridge import apps_mcp as A  # noqa: E402
from eve_bridge import github_repos as G  # noqa: E402

rng = random.Random(20261020)

# ---- names_app ------------------------------------------------------------------------------------------
WORDS = ("mail emails e-mail emailed inbox gmail unread newsletter receipts sent me write back to him reply to that "
         "message thread contacts contact phone numbers my your email address addresses his her their number "
         "sam's ana’s cell number for please pls can you could and then also send an email to write me it "
         "the this that from about back is was for i calendar meetings event appointment agenda rescheduled busy "
         "am are we free at on tomorrow today tonight later / availability invites rsvp book a slot class classes "
         "lecture labs section seminars office hours exams midterm finals finally quiz quizzes flight what's whats "
         "next up my day drive to there home over google docs doc documents sheet spreadsheets slides folders pdf "
         "github gh repo repos repository pull requests pr prs commits branch branches ci builds build pass passed "
         "fail broke green red status workflows actions action run runs failed passed issue issues linear notion "
         "slack jira uber eats ubereats instacart order orders delivery deliveries maps map link directions "
         "navigate route routes how long far get getting driving walk biking cycle commute traffic eta nearby near "
         "here closest nearest where where's is location weather forecast rain raining restaurants cafe café cafés "
         "coffee food bars lunch dinner brunch around joke tell remind merge notes Email Please MAIL Where’s "
         "İnbox ünread ".split())
texts = [
    "any mail from Sam?", "what's on my calendar tomorrow", "find the budget sheet in my drive", "open PRs on eve",
    "how long to SFO by car", "what's the weather like", "where's my next lecture", "did CI pass on eve",
    "what is my email address", "email sam that I'm late", "Please Email Bob the doc", "pls email ana",
    "send an email to bob about it", "write an email to Ana", "email me", "your email address", "my e-mail address",
    "his number", "Sam’s cell", "number for the dentist", "drive home", "drive", "drive over", "Drive to SF",
    "the cafe nearby", "café near me", "e-mail address", "email address of the hotel", "address of the hotel",
    "any issues with my order", "issues on eve", "the build is green", "my build", "build me", "actions failed",
    "finally", "finals week", "is Bob free", "free/busy", "free / busy", "", "   ", "email", "e-mails sent me",
]
for _ in range(2500):
    n = rng.randint(1, 9)
    t = " ".join(rng.choice(WORDS) for _ in range(n))
    if rng.random() < 0.3:
        t = t.upper() if rng.random() < 0.5 else t.capitalize()
    if rng.random() < 0.3:
        t += rng.choice(["?", ".", "!", "...", ",", " :)"])
    texts.append(t)
A.REPO_NAMES = frozenset()
names_app = [[t, sorted(app for app in list(A.APP_WORDS) if A.names_app(app, t))] for t in texts]

# ---- tool names --------------------------------------------------------------------------------------------
PARTS = (list(A.READ_VERBS) + list(A.WRITE_VERBS)[:40] + ["thread", "event", "file", "issue", "draft", "label",
         "order", "run", "and", "ui", "x", "Get", "LIST", "pin", "star", "lock", "schedule", "frobnicate"])
tool_reads = []
for _ in range(800):
    name = rng.choice(["_", "-"]).join(rng.choice(PARTS) for _ in range(rng.randint(1, 4)))
    tool = {"name": name}
    r = rng.random()
    if r < 0.4:
        tool["annotations"] = {"readOnlyHint": True}
    elif r < 0.6:
        tool["annotations"] = {"readOnlyHint": rng.choice([False, 1, "true", None])}
    elif r < 0.7:
        tool["annotations"] = rng.choice([[], "x", None])
    tool_reads.append([tool, A.tool_reads(tool), A.name_reads(name)])

# ---- routine_write -----------------------------------------------------------------------------------------
KEYS = ["summary", "title", "description", "location", "startTime", "start_time", "endTime", "start", "end",
        "allDay", "timeZone", "recurrence", "calendarId", "calendar_id", "attendeeEmails", "addAttendeeEmails",
        "attendees", "notifyGuests", "notify_guests", "sendUpdates", "send_updates", "sendNotifications", "eventId",
        "event_id", "text", "to", "cc", "body", "subject", "draftId", "send", "name", "private", "PRIVATE", "autoInit",
        "auto_init", "organization", "parents", "permissions", "textContent", "mimeType", "file_to_upload",
        "folder_id", "content"]
VALUES = [None, False, True, 0, 1, 0.0, "none", "all", "primary", "", "x", "a@b.co", "Lunch Friday", [], ["a@b.co"],
          {"date": "2026-10-10"}, {"dateTime": "x", "timeZone": "y"}, {"dateTime": "x", "other": 1}, {}, "shared"]
APPS = ["gmail", "calendar", "drive", "docs", "sheets", "github", "googlecalendar", "GoogleDrive", "googledocs",
        "googlesheets", "slack", "Calendar"]
TOOLS = ["create_draft", "update_draft", "create_email_draft", "draft_reply", "send_message", "create_file",
         "upload_file", "create_folder", "UPLOAD_FILE", "create_document", "CREATE_SPREADSHEET", "create_repository",
         "quick_add_event", "create_event", "update_event", "patch_event", "events_insert", "EVENTS_PATCH",
         "UPDATE_EVENT", "delete_event", "respond_to_event", "create-event"]
routine = []
for _ in range(4000):
    app, tool = rng.choice(APPS), rng.choice(TOOLS)
    r = rng.random()
    if r < 0.05:
        args = rng.choice([None, ["x"], "x"])
    else:
        args = {k: rng.choice(VALUES) for k in rng.sample(KEYS, rng.randint(0, 4))}
    routine.append([app, tool, args, A.routine_write(app, tool, args)])

# ---- describing an act ----------------------------------------------------------------------------------------
STRS = ["eve", "t" * 200, "a  b\n c", "café", "x‮y", "​hidden", "line\nbreak", "", "Ana <a@b.co>",
        "word " * 80, "\U0001f600 emoji"]
VALS = STRS + [None, True, 3, 2.5, 1e16, ["a", "b"], {"k": "v", "n": [1, None]}, [], {}]
NAMES = ["github__create_issue", "drive__copy_file", "gmail__send_message", "calendar__update_event", "slack__send",
         "maps__compute_routes", "calendar__"]
describe = []
for _ in range(400):
    name = rng.choice(NAMES)
    keys = rng.sample(["to", "cc", "bcc", "subject", "body", "replyToMessageId", "repo", "title", "labels"],
                      rng.randint(0, 5))
    args = {k: rng.choice(VALS) for k in keys}
    describe.append([name, args, A.describe_for_confirmation(name, args), A.describe_whole(name, args, 500),
                     A.describe_whole(name, args, A.WHOLE_ASK_CHARS)])

# ---- results ---------------------------------------------------------------------------------------------------
results = []
for _ in range(300):
    res = {}
    if rng.random() < 0.5:
        res["isError"] = rng.choice([True, False, 1, 0, None, "yes"])
    if rng.random() < 0.3:
        res["structuredContent"] = rng.choice([{"a": 1}, None, [1, "é"], "s"])
    if rng.random() < 0.8:
        res["content"] = [rng.choice([{"type": "text", "text": rng.choice(STRS)}, {"type": "image", "data": "x"},
                                      {"type": "text"}, {"text": "no type"}, "junk", {"type": "text", "text": None}])
                          for _ in range(rng.randint(0, 4))]
    results.append([res, A.result_for_model(res)])
for res in [{"structuredContent": {"big": "y" * 61000}}, {"isError": True, "content": [
        {"type": "text", "text": "z" * 30500}, {"type": "text", "text": "\u00e9" * 30000}]}]:
    results.append([res, A.result_for_model(res)])

# ---- schemas -------------------------------------------------------------------------------------------------
schemas = []
for _ in range(200):
    def node(depth: int):
        n = {"type": rng.choice(["object", "string", "array", "integer"])}
        if rng.random() < 0.5:
            n["description"] = rng.choice(STRS + [None, 5, ["a"]])
        for k in rng.sample(["title", "x-vendor", "$schema", "examples", "deprecated", "default", "enum"],
                            rng.randint(0, 3)):
            n[k] = rng.choice([True, "v", [1], {"a": 1}])
        if depth < 3 and rng.random() < 0.6:
            n["properties"] = {f"p{i}": node(depth + 1) for i in range(rng.randint(0, 3))}
        if depth < 3 and rng.random() < 0.3:
            n["items"] = node(depth + 1)
        if depth < 3 and rng.random() < 0.2:
            n["anyOf"] = [node(depth + 1), "s"]
        return n
    schema = node(0)
    tool = {"name": rng.choice(["list_events", "x" * 70, "a"]), "inputSchema": rng.choice([schema, schema, None, {}])}
    if rng.random() < 0.7:
        tool["description"] = rng.choice(STRS + [None, 0])
    app = rng.choice(["calendar", "maps", "github", "gmail"])
    schemas.append([app, tool, schema, A.slim_schema(schema), A.function_tool(app, tool)])

# ---- open_app, policy, scopes, clip, sse ------------------------------------------------------------------
asked = []
for args in [{"apps": ["calendar", " Maps ", "", None, "maps", 3, True, 0, [], ["x"]], "app": "Gmail"},
             {"apps": "github"}, {"apps": None, "app": ""}, {"app": "Σίσυφος"}, {"apps": {"a": 1}}, {}, {"apps": 5},
             {"apps": ["İstanbul", "MAPS"]}]:
    asked.append([args, A.open_asked(args)])
policies = []
for raw in ["", "github=write, drive=read, maps=never, =x", "GMAIL=READ", "slack=ask,gmail = read,calendar=ask=",
            "drive=read,drive=write", ",,,", "contacts=auto;maps=write", "Ｇmail=read"]:
    pol = A.policy({"EVE_APPS_POLICY": raw})
    policies.append([raw, list(pol.items()), A.google_scopes(pol)])
clips = [[t, n, A._clip(t, n)] for t in STRS + ["  lead and trail  ", "a　b\x1cc"] for n in (1, 5, 30, 400)]
sse = []
for body in ['data: {"id": 1,\ndata:  "result": {}}\n\ndata: not json\n\nevent: x\ndata: {"id": 2}\n',
             "data:{\"a\":1}\r\n\r\ndata: [1]\n\n", "data: {\"x\": 1}  data: {\"y\": 2}", ": c\n\n",
             "data: {\"id\": 3}\x0b\x0bdata:\t{\"z\": 1}\n \n", "data: \"str\"\n\n"]:
    sse.append([body, A._sse_messages(body)])
confs = []
for env in [{}, {"EVE_APPS": "1", "EVE_APPS_TIMEOUT_SECS": "-3"}, {"EVE_APPS": " Yes ", "EVE_APPS_TIMEOUT_SECS": "1_5"},
            {"EVE_APPS": "on", "EVE_APPS_TIMEOUT_SECS": " 2.5e1 "}, {"EVE_APPS": "2", "EVE_APPS_TIMEOUT_SECS": "nan"},
            {"EVE_APPS_TIMEOUT_SECS": "abc", "EVE_APPS_DIR": "/tmp/x"}, {"EVE_APPS_TIMEOUT_SECS": "0"}]:
    c = A.configured(env)
    confs.append([env, c.enabled, c.timeout_secs, str(c.config_dir)])

# ---- files -----------------------------------------------------------------------------------------------------
files = []
with tempfile.TemporaryDirectory() as d:
    for data in [{"refresh_token": "r", "expires_at": 1760000000.123456, "scope": "a b"},
                 {"fetched": 9e12, "repos": [{"name": "x", "description": "café \U0001f600"}], "e": [], "o": {}},
                 {"n": [1, 2.5, None, True, {"k": []}], "s": "q\"\\\n\t\x7f\x01"}, {}]:
        p = Path(d) / "t.json"
        A.write_private_json(p, data)
        files.append([data, p.read_text(encoding="utf-8")])

# ---- github_repos ---------------------------------------------------------------------------------------------
repo_names = ["zorbcar", "remix", "qbt", "plantpal-x", "Leap", "eve", "notes", "my.repo", "_hidden", "café-app",
              "A1B2", "storeybox", "dotfiles", "x" * 60, "test-bed", "abcd", "ZORBCAR2", "web_site", "-dash"]
repos = [{"name": n, "description": rng.choice(["", "a thing", "x" * 80, "  spaced   out  ", "café " * 20])}
         for n in repo_names]
keyword = sorted(G.keyword_names(repos))
mention_texts = ["any new commits on zorbcar?", "is plantpal-x green", "Check zorbcar.", "look at plantpal-x...",
                 "zorbcars", "ZORBCAR2 status", "my.repo is down", "test-bed!", "abcd_", "on a1b2-", "remix", "",
                 "plantpal-x.", "the my.repo.", "zorbcar-", "web_site"]
mentions = [[t, G.names_repo(t, frozenset(keyword))] for t in mention_texts]
line = [G.line(repos), G.line(repos[:3]), G.line([])]

json.dump({"names_app": names_app, "tool_reads": tool_reads, "routine": routine, "describe": describe,
           "results": results, "schemas": schemas, "asked": asked, "policies": policies, "clips": clips, "sse": sse,
           "configured": confs, "files": files, "repos": repos, "keyword": keyword, "mentions": mentions,
           "line": line},
          open(HERE / "python_parity.json", "w", encoding="utf-8"), ensure_ascii=False)
print("wrote", HERE / "python_parity.json")
