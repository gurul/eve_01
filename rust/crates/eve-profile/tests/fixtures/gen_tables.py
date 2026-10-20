"""Writes tables.json: profile.py's tables as Python builds them, for tests/tables.rs to compare byte for byte.

Run from the repository root: python3 rust/crates/eve-profile/tests/fixtures/gen_tables.py
"""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "bridge" / "src"))
from eve_bridge import profile  # noqa: E402

tables = {
    "LITE_FORCED": list(profile.LITE_FORCED.items()),
    "LITE_DEFAULTS": list(profile.LITE_DEFAULTS.items()),
    "LITE_CLASSIFIED": list(profile.LITE_CLASSIFIED.items()),
    "NOT_SECRET_NAMES": list(profile.NOT_SECRET_NAMES.items()),
    "LITE_SECRET_ALLOW": sorted(profile.LITE_SECRET_ALLOW),
    "SECRET_SUFFIXES": list(profile.SECRET_SUFFIXES),
    "SECRET_NAMES": sorted(profile.SECRET_NAMES),
    "LITE_TOOL_NAMES": sorted(profile.LITE_TOOL_NAMES),
    "LITE_TOOL_PREFIXES": list(profile.LITE_TOOL_PREFIXES),
    "EXEC_ALLOW": sorted(profile.EXEC_ALLOW),
    "PROTECTED_HOME": list(profile.PROTECTED_HOME),
    "PROTECTED_ROOTS": list(profile.PROTECTED_ROOTS),
    "FIREWALL_MODES": list(profile.FIREWALL_MODES),
    "protected_roots": profile.protected_roots({"HOME": "/Users/someone"}),
}
out = pathlib.Path(__file__).with_name("tables.json")
out.write_text(json.dumps(tables, indent=1, ensure_ascii=False) + "\n")
