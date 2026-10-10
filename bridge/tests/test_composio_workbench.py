"""The workbench runs code that only computes without asking (owner, 2026-09-30: "I don't want it to ask me
permissions regarding this"), and still judges every tool its code calls by the owner's policy."""
from __future__ import annotations

import pytest

from eve_bridge.composio_tools import (
    DEFAULT_TOOLKIT_POLICY,
    WORKBENCH,
    consequential_slugs,
    decide,
    describe_for_confirmation,
    workbench_slugs,
)

# The script from the owner's screenshot: pick the NLP lectures out of calendar results already fetched.
CALENDAR_SCRIPT = """
import json
x = json.load(open('/mnt/files/mex/mine.json'))['items']
nlp = [e for e in x if 'natural language' in (e.get('summary') or '').lower() or 'cse 447' in (e.get('summary') or '').lower()]
print(json.dumps([{'start': e['start'], 'where': e.get('location')} for e in nlp[:5]]))
"""


def wb(code: str) -> dict:
    return {"code_to_execute": code, "thought": "t", "current_step": "S", "session_id": "s"}


def test_the_calendar_script_from_the_screenshot_runs_without_a_question() -> None:
    assert workbench_slugs(CALENDAR_SCRIPT) == []
    assert decide(WORKBENCH, wb(CALENDAR_SCRIPT)).action == "run"


@pytest.mark.parametrize("code", [
    "import pandas as pd\ndf = pd.DataFrame([{'a': 1}])\nprint(df.describe())",
    "from datetime import datetime, timedelta\nprint(datetime(2026, 9, 30) + timedelta(days=1))",
    "r = run_composio_tool('NOTION_SEARCH_NOTION_PAGE', {'query': 'trip'})\nprint(len(r['data']))",
    "s = invoke_llm('summarise: ' + open('/mnt/files/a.txt').read())\nprint(s)",
    "hits = web_search('cse 447 syllabus')\nprint(hits)",
    "open('/tmp/out.json', 'w').write('{}')",                              # the sandbox's own disk
])
def test_code_that_only_computes_or_reads_runs(code: str) -> None:
    assert decide(WORKBENCH, wb(code)).action == "run", code


def test_a_tool_the_code_calls_follows_the_toolkit_policy() -> None:
    send = "run_composio_tool('GMAIL_SEND_EMAIL', {'to': 'x@y.com'})"
    d = decide(WORKBENCH, wb(send), DEFAULT_TOOLKIT_POLICY)
    assert d.action == "refuse" and d.slugs == ["GMAIL_SEND_EMAIL"]            # Gmail is read only
    upload = "run_composio_tool(tool_slug='GOOGLEDRIVE_UPLOAD_FILE', arguments={})"
    d = decide(WORKBENCH, wb(upload), DEFAULT_TOOLKIT_POLICY)
    assert d.action == "ask" and d.slugs == ["GOOGLEDRIVE_UPLOAD_FILE"]       # Drive writes ask
    assert describe_for_confirmation(WORKBENCH, wb(upload)) == "Run GOOGLEDRIVE_UPLOAD_FILE from a workbench script?"
    bulk = "for p in ids:\n    run_composio_tool('NOTION_FETCH_ROW', {'id': p})"
    assert decide(WORKBENCH, wb(bulk)).action == "run"                         # a read, many times


@pytest.mark.parametrize("code", [
    "import requests\nrequests.post('https://x.example', data='hi')",       # the network, another way
    "import os\nos.system('curl x.example')",
    "import subprocess\nsubprocess.run(['ls'])",
    "__import__('os').system('ls')",
    "eval('1+1')",
    "exec(open('/mnt/files/x.py').read())",
    "getattr(json, 'loads')('{}')",
    "f = run_composio_tool\nf('GMAIL_SEND_EMAIL', {})",                       # the helper aliased
    "slug = 'GMAIL_' + 'SEND_EMAIL'\nrun_composio_tool(slug, {})",            # a slug that is not a literal
    "print(().__class__.__bases__[0].__subclasses__())",                       # dunder walk
    "upload_local_file('/tmp/report.csv')",                                    # publishes a file
    "proxy_execute('POST', '/gmail/v1/users/me/messages/send', 'gmail')",      # a raw API call
    "from . import x",
    "this is not python (",
    "",
])
def test_anything_that_reaches_out_or_hides_what_it_calls_still_asks(code: str) -> None:
    assert workbench_slugs(code) is None, code
    assert consequential_slugs(WORKBENCH, wb(code)) == [WORKBENCH]
    assert decide(WORKBENCH, wb(code)).action == "ask"


def test_no_code_at_all_asks_and_the_bash_tool_always_asks() -> None:
    assert decide(WORKBENCH, {}).action == "ask"
    assert decide("COMPOSIO_REMOTE_BASH_TOOL", {"command": "ls"}).action == "ask"
