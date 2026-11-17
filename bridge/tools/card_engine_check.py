#!/usr/bin/env python3
"""Which card format an app turn is told to write, for each setting of EVE_CARD_ENGINE, read from the whole request
the brain would get (telegram.request, the app style and the app block included), not from the constants.

    uv run python tools/card_engine_check.py

Prints CARD_ENGINE_SWITCH_OK only when all of these hold:
- unset, empty or an unknown value: the instructions name the OpenUI block and never the json-render one;
- json-render: the instructions name the json-render block and never the OpenUI one (no "openui" in any case);
- eve lite's instructions hold the same two ways, and never name the phone app;
- a text turn (no app block) names neither.
Each absence is checked beside its own presence in the other setting, so a search that could not find the name
anywhere fails here too.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
os.environ.setdefault("EVE_ENV_FILE", "/nonexistent-env-file")
os.environ.pop("EVE_PROFILE", None)

from eve_bridge import link_chat, telegram  # noqa: E402

OPENUI, JSON = "openui", "json-render"


def instructions(app: bool) -> str:
    cfg = telegram.configured({"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "0:x", "EVE_TELEGRAM_OWNER": "1"})
    items = [telegram.message_item("user", "hi")]
    return str(telegram.request(cfg, items, app_block=link_chat.app_block() if app else "")["instructions"])


def main() -> int:
    bad: list[str] = []

    def check(name: str, ok: bool) -> None:
        print(f"{'pass' if ok else 'FAIL'}  {name}")
        if not ok:
            bad.append(name)

    for lite in (False, True):
        if lite:
            os.environ["EVE_PROFILE"] = "lite"
        else:
            os.environ.pop("EVE_PROFILE", None)
        who = "lite" if lite else "full"
        for value in (None, "", "something-else", "openui"):
            if value is None:
                os.environ.pop(link_chat.CARD_ENGINE_ENV, None)
            else:
                os.environ[link_chat.CARD_ENGINE_ENV] = value
            text = instructions(app=True).lower()
            check(f"{who}, EVE_CARD_ENGINE={value!r}: names the OpenUI block ({text.count('openui-lang')} times), "
                  f"never json-render ({text.count(JSON)})", "```openui-lang" in text and JSON not in text)
        for value in ("json-render", "JSON_RENDER", " json-render "):
            os.environ[link_chat.CARD_ENGINE_ENV] = value
            text = instructions(app=True).lower()
            check(f"{who}, EVE_CARD_ENGINE={value!r}: names the json-render block ({text.count(JSON)} times), "
                  f"never OpenUI ({text.count(OPENUI)})", "```json-render" in text and OPENUI not in text)
            if lite:
                check(f"lite, EVE_CARD_ENGINE={value!r}: never names the phone app", "eve link" not in text
                      and "iphone" not in text)
            plain = instructions(app=False).lower()
            check(f"{who}, EVE_CARD_ENGINE={value!r}: a text turn names neither format",
                  OPENUI not in plain and JSON not in plain)
    os.environ.pop("EVE_PROFILE", None)
    os.environ.pop(link_chat.CARD_ENGINE_ENV, None)
    if bad:
        print(f"NOT OK: {len(bad)} check(s) failed")
        return 1
    print("CARD_ENGINE_SWITCH_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
