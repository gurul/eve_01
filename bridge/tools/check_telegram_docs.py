"""Gate: the Telegram door's docs say what the code reads.

The names are read out of telegram.py, not listed here, so a new EVE_TELEGRAM* knob that nobody
documented fails this. Prints TELEGRAM_DOCS_OK only after every assertion holds.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "bridge/src/eve_bridge/telegram.py"
DOC = ROOT / "docs/stackchan/telegram.md"


def main() -> int:
    source, doc, readme = SOURCE.read_text(), DOC.read_text(), (ROOT / "README.md").read_text()
    records_source = (ROOT / "bridge/src/eve_bridge/records.py").read_text()
    names = set(re.findall(r"EVE_TELEGRAM[A-Z_]*", source)) | set(re.findall(r"EVE_RECORDS[A-Z_]*", records_source))
    # the door's companions (2026-09-21): the apps, the search engine, the Auto Mode gate, the second brain
    for module, prefix in (("composio_tools.py", r"EVE_COMPOSIO[A-Z_]*"), ("apps_mcp.py", r"EVE_(?:APPS|GITHUB_MCP|GOOGLE_MAPS)[A-Z_]*"),
                           ("websearch.py", r"EVE_WEB_SEARCH[A-Z_]*"),
                           ("daemon.py", r"EVE_COMMAND_RISK[A-Z_]*"),
                           ("second_brain.py", r"EVE_(?:SECOND_BRAIN|VAULT)[A-Z_]*"),
                           # and file sharing (2026-10-04)
                           ("share.py", r"EVE_SHARE[A-Z_]*"),
                           # and which toolsets a turn starts with (2026-10-05)
                           ("toolset_router.py", r"EVE_TOOLSET_ROUTER[A-Z_]*"),
                           # and the chat log (2026-10-08)
                           ("chat_log.py", r"EVE_CHAT_LOG[A-Z_]*")):
        path = ROOT / "bridge/src/eve_bridge" / module
        if path.exists():
            names |= set(re.findall(prefix, path.read_text()))
    names = sorted(names)
    problems: list[str] = []
    if len(names) < 8:
        problems.append(f"found only {names}: the reader is broken, not the docs")
    problems += [f"{name} is read by telegram.py but not documented in {DOC.name}" for name in names
                 if not re.search(rf"`{name}`", doc)]
    if "telegram.py" not in readme or "docs/stackchan/telegram.md" not in readme:
        problems.append("README.md does not name telegram.py and link docs/stackchan/telegram.md")
    if "telegram-check" not in doc:
        problems.append("the setup command `eve-bridge telegram-check` is not in the doc")
    deps = tomllib.loads((ROOT / "bridge/pyproject.toml").read_text())["project"]["dependencies"]
    if re.search(r"^\s*import httpx|^\s*from httpx", source, re.M) and not any(d.startswith("httpx") for d in deps):
        problems.append("telegram.py imports httpx but pyproject.toml does not declare it")
    if not re.search(r"^TELEGRAM_DEFAULT = False\b", source, re.M):
        problems.append("TELEGRAM_DEFAULT is not False: the docs say it ships off")
    # records.py has had no switch of its own since ba746a7 (the records are part of memory, EVE_MEMORY): its
    # old RECORDS_DEFAULT check failed on main from then on (found 2026-09-29, chief P4). A EVE_RECORDS* name
    # it reads again must still be documented (``names`` above).
    if "records.py" not in readme or "memory_search" not in doc:
        problems.append("the records layer is not in README.md / the doc")
    for line in problems:
        print("FAIL:", line)
    if problems:
        return 1
    print(f"{len(names)} names documented: {', '.join(names)}")
    print("TELEGRAM_DOCS_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
