"""Write python_verdicts.json: composio_tools.py's own answers over the owner golden's decide() corpus plus the
workbench scripts the Python tests use and a few that pin ast.walk's breadth-first order. The Rust tests replay
the file and must agree with every line.

Run from the repository root with eve's venv (it only reads bridge/src and bridge/tools):

    bridge/.venv/bin/python rust/crates/eve-composio-tools/tests/fixtures/make_python_verdicts.py
"""

from __future__ import annotations

import ast
import json
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[5]
sys.path[:0] = [str(ROOT / "bridge" / "src")]

from eve_bridge import composio_tools as c  # noqa: E402

GOLDEN = ROOT / "bridge" / "tools" / "owner_golden.py"
OUT = Path(__file__).with_name("python_verdicts.json")


def golden_cases() -> tuple[dict[str, str], list[tuple[str, str, Any]]]:
    """DECIDE_CASES and POLICIES from owner_golden.py, without importing the whole tool (it imports the daemon)."""
    src = GOLDEN.read_text(encoding="utf-8")
    start = src.index("M, W = composio_tools.MULTI_EXECUTE")
    end = src.index("# Names beyond the brain's own lists")
    ns: dict[str, Any] = {"composio_tools": c, "Any": Any, "Mapping": dict, "Optional": Any}
    exec(compile("from __future__ import annotations\n" + src[start:end], str(GOLDEN), "exec"), ns)  # noqa: S102
    return ns["POLICIES"], list(ns["DECIDE_CASES"])


def bench(code: str) -> dict[str, Any]:
    return {"code_to_execute": code, "thought": "t", "current_step": "S", "session_id": "s"}


SCRIPTS = [
    # breadth-first order: the call nested deeper comes later even when it is earlier in the source
    "for p in ids:\n    run_composio_tool('SLACK_SEND_MESSAGE', {'id': p})\nx = run_composio_tool('NOTION_CREATE_PAGE', {})",
    "x = f(run_composio_tool('LINEAR_CREATE_ISSUE', {}))\nrun_composio_tool('SLACK_SEND_MESSAGE', {})",
    "run_composio_tool('SLACK_SEND_MESSAGE', run_composio_tool('NOTION_CREATE_PAGE', {}))",
    "if a:\n    run_composio_tool('SLACK_SEND_MESSAGE', {})\nelif b:\n    run_composio_tool('NOTION_CREATE_PAGE', {})\n"
    "else:\n    run_composio_tool('LINEAR_CREATE_ISSUE', {})\nrun_composio_tool('SLACK_ADD_REACTION', {})",
    "def f(x=run_composio_tool('SLACK_SEND_MESSAGE', {})):\n    return run_composio_tool('NOTION_CREATE_PAGE', {})\n"
    "@d(run_composio_tool('LINEAR_CREATE_ISSUE', {}))\ndef g(): pass",
    "d = {run_composio_tool('SLACK_SEND_MESSAGE', {}): 1, **e, 'k': run_composio_tool('NOTION_CREATE_PAGE', {})}",
    "y = [run_composio_tool('SLACK_SEND_MESSAGE', {}) for i in run_composio_tool('NOTION_FETCH_ROW', {}) if i]",
    "s = f\"{run_composio_tool('SLACK_SEND_MESSAGE', {})!r:{w}}\" 'tail'",
    "with open('a') as f, g(run_composio_tool('NOTION_CREATE_PAGE', {})):\n    run_composio_tool('SLACK_SEND_MESSAGE', {})",
    "try:\n    run_composio_tool('SLACK_SEND_MESSAGE', {})\nexcept E as e:\n    run_composio_tool('NOTION_CREATE_PAGE', {})\n"
    "finally:\n    pass",
    "match x:\n    case {'a': 1, **rest} if run_composio_tool('SLACK_SEND_MESSAGE', {}):\n        pass\n"
    "    case P(a, b=1):\n        run_composio_tool('NOTION_CREATE_PAGE', {})",
    "class C(B, metaclass=run_composio_tool('SLACK_SEND_MESSAGE', {})):\n    x: int = run_composio_tool('NOTION_CREATE_PAGE', {})",
    "lam = lambda a, *b, c=run_composio_tool('SLACK_SEND_MESSAGE', {}), **d: a\nprint(lam)",
    "run_composio_tool('GMAIL_' 'SEND_EMAIL', {})",
    "run_composio_tool(f'GMAIL_SEND_EMAIL', {})",
    "run_composio_tool(b'GMAIL_SEND_EMAIL', {})",
    "run_composio_tool('   ', {})",
    "run_composio_tool(  ' SLACK_SEND_MESSAGE ', {})",
    "run_composio_tool(action='SLACK_SEND_MESSAGE')",
    "run_composio_tool(arguments={}, slug='SLACK_SEND_MESSAGE')",
    "run_composio_tool(*args)",
    "run_composio_tool()",
    "x.run_composio_tool('SLACK_LIST_MESSAGES', {})",
    "import json, os.path",
    "import json.decoder as jd\nfrom collections import abc",
    "from collections.abc import Mapping as _M",
    "import numpy as np\nnp.load('x.npy', allow_pickle=True)",
    "import pandas as pd\npd.read_pickle('p.pkl')",
    "data = cPickle_like = 1",
    "x = obj.__class__",
    "print(ｅｖａｌ('1'))",
    "def f[T](x: T) -> T:\n    return x\nprint(f(1))",
    "type Alias = list[int]\nprint(1)",
    "print(f\"{'a'}\" + f\"{x['k']}\")",
    "print(f\"{x[\"k\"]}\")",
    "global os\nprint(1)",
    "except_ = 1\ntry:\n    pass\nexcept* ValueError:\n    pass",
    "t = t'hi {name}'",
    "try:\n    pass\nexcept A, B:\n    pass",
    "x = 1\x00",
    "async def f():\n    async with a as b:\n        await run_composio_tool('SLACK_SEND_MESSAGE', {})\n    async for i in run_composio_tool('NOTION_FETCH_ROW', {}):\n        pass",
    "x = (yield)\nnonlocal_ = 2",
    "print(*[1], sep='')\nprint(x[1:2:3, ...])",
    "def g(a, /, b, *, c, d=run_composio_tool('SLACK_SEND_MESSAGE', {})): pass",
    "import operator\noperator.itemgetter('a')",
    "from operator import attrgetter",
    "import functools\nfunctools.reduce(lambda a, b: a + b, [1, 2])",
    "import requests.adapters",
    "from typing import get_type_hints as g",
    "import base64\nbase64.b64decode('aGk=')",
    "q = df.query('a > 1')",
    "w = web_search('x')\ninvoke_llm(w)",
    "while run_composio_tool('SLACK_FETCH_HISTORY', {}):\n    break\nelse:\n    run_composio_tool('SLACK_SEND_MESSAGE', {})",
    "del x[run_composio_tool('SLACK_SEND_MESSAGE', {})]",
    "assert run_composio_tool('SLACK_SEND_MESSAGE', {}), run_composio_tool('NOTION_CREATE_PAGE', {})",
    "raise E(run_composio_tool('SLACK_SEND_MESSAGE', {})) from run_composio_tool('NOTION_CREATE_PAGE', {})",
    "z = {run_composio_tool('SLACK_SEND_MESSAGE', {}): v for v in run_composio_tool('NOTION_FETCH_ROW', {})}",
    "(a := run_composio_tool('SLACK_SEND_MESSAGE', {}))",
    "x = a if run_composio_tool('SLACK_SEND_MESSAGE', {}) else run_composio_tool('NOTION_CREATE_PAGE', {})",
    "x = not run_composio_tool('SLACK_SEND_MESSAGE', {}) or (1 < run_composio_tool('NOTION_CREATE_PAGE', {}) <= 3)",
    "x = {*run_composio_tool('SLACK_SEND_MESSAGE', {})}\ny = (*a, run_composio_tool('NOTION_CREATE_PAGE', {}))",
]

EXTRA_CALLS: list[tuple[str, Any]] = [
    (c.MULTI_EXECUTE, None),
    (c.MULTI_EXECUTE, "opaque"),
    (c.MULTI_EXECUTE, {"tools": None}),
    (c.MULTI_EXECUTE, {"tools": [{}]}),
    (c.MULTI_EXECUTE, {"tools": [None]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": ""}]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": 7}]}),
    (c.MULTI_EXECUTE, {"tools": []}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "account": 5}]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "account": " Work "},
                                 {"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {"text": "x\u202ey", "n": 1.5,
                                                                                   "big": 1e20, "none": None,
                                                                                   "nested": {"a": ["é", 2]}}}]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL\n", "arguments": {}, "account": "work"}]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": "  NOTION_CREATE_PAGE  ", "arguments": {"title": "   spaced\t\tout  "}}]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": f"APP_CREATE_THING_{i}", "arguments": {"v": "w" * 70}}
                                 for i in range(12)]}),
    (c.MULTI_EXECUTE, {"tools": [{"tool_slug": "EXA_SEARCH"}, {"tool_slug": "tavily_mcp_search"}]}),
    ("COMPOSIO_REMOTE_BASH_TOOL", {"command": "ls\n-la"}),
    ("COMPOSIO_SEARCH_TOOLS", ["not", "a", "mapping"]),
    (c.MANAGE_CONNECTIONS, {"toolkits": ["gmail"], "reinitiate_all": False, "session_id": None}),
]


def answers(policies: dict[str, Any], name: str, args: Any) -> dict[str, Any]:
    out: dict[str, Any] = {"name": name, "args": args, "verdicts": {}, "lite": {}}
    for pn, p in policies.items():
        d = c.decide(name, args, p)
        out["verdicts"][pn] = [d.action, list(d.slugs), d.why]
        dl = c.decide_lite(name, args, p)
        out["lite"][pn] = [dl.action, list(dl.slugs), dl.why]
    out["ask_line"] = c.describe_for_confirmation(name, args)
    out["whole_1500"] = c.describe_whole(name, args, 1500)
    out["consequential"] = c.consequential_slugs(name, args)
    out["sends_mail"] = c.sends_mail(name, args)
    out["web"] = c.web_toolkit_slugs(name, args)
    out["manage_needs_yes"] = c.manage_needs_yes(args)
    if name == c.WORKBENCH and isinstance(args, dict):
        out["workbench_slugs"] = c.workbench_slugs(args.get("code_to_execute"))
        out["reaches_out_lite"] = c.workbench_reaches_out_lite(args.get("code_to_execute"))
    return out


def main() -> None:
    raw_policies, cases = golden_cases()
    policies = {n: (c.toolkit_policy({"EVE_COMPOSIO_POLICY": r}) if r else None) for n, r in raw_policies.items()}
    rows = [dict(answers(policies, name, args), key=key) for key, name, args in cases]
    rows += [dict(answers(policies, c.WORKBENCH, bench(s)), key=f"script {i}") for i, s in enumerate(SCRIPTS)]
    rows += [dict(answers(policies, n, a), key=f"extra {i}") for i, (n, a) in enumerate(EXTRA_CALLS)]
    OUT.write_text(json.dumps({"policies": raw_policies, "cases": rows}, indent=1, ensure_ascii=False) + "\n",
                   encoding="utf-8")
    print(f"{len(rows)} cases -> {OUT}")


if __name__ == "__main__":
    main()
