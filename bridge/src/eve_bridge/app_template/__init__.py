"""`eve-bridge app new <name>` and `eve-bridge app list` (docs/adding-an-app.md).

``scaffold`` writes a new app that passes its own tests as generated: the module (``bridge/src/eve_bridge/<name>.py``,
an on/off switch kept in memory, two tools and a generic phone sheet), its test (``bridge/tests/test_<name>.py``),
its doc (``docs/<name>.md``), and one line in ``app_registry.APPS`` above the marker. The app is off by default
(``EVE_<NAME>=1`` turns it on), so writing one changes nothing the models read. A refused name writes nothing.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from string import Template
from typing import Optional

from .. import app_registry

HERE = Path(__file__).resolve().parent

REGISTRY_LINE = Template(
    '    # $name: set symbol (an SF Symbol) and detail (the phone row\'s subtitle); if you change its tools, change\n'
    '    # acting and clean to match (docs/adding-an-app.md, "Change the tools")\n'
    '    App(slot="$name", module="$name", make="make", switch="$SWITCH", default_on=False,\n'
    '        title="$Name", symbol="square.grid.2x2", detail="", doc="docs/$name.md",\n'
    '        no_control="$Name is off on this Mac ($SWITCH).", label="$name", tool_prefix="${name}_",\n'
    '        acting=frozenset({"${name}_set"}), clean=frozenset({"${name}_status"})),\n')


class Refused(ValueError):
    """The name cannot be a new app; nothing was written."""


def find_root(start: Optional[Path] = None) -> Path:
    """The repository root: the first folder up from ``start`` (the working folder) with bridge/pyproject.toml."""
    here = (start or Path.cwd()).resolve()
    for folder in (here, *here.parents):
        if (folder / "bridge" / "pyproject.toml").is_file():
            return folder
    raise Refused("run this inside eve's repository (no bridge/pyproject.toml above this folder)")


def own_prefixes() -> frozenset[str]:
    """The first word of each of eve's own tools (the tool gate's sets), so a new app's tools never share one."""
    from .. import tool_gate

    names = (set(tool_gate.ACTING) | set(tool_gate.CLEAN) | set(tool_gate._SOURCES) | set(tool_gate._SAY_HEADS)) \
        - {n for app in app_registry.APPS for n in (*app.acting, *app.clean, *app.sources, *app.say)}
    return frozenset(n.split("_")[0] + "_" for n in names if "_" in n)


def own_names() -> frozenset[str]:
    """eve's own tool names (the tool gate's sets), whole: a new app's name must not be one (``screenshot``)."""
    from .. import tool_gate

    return frozenset(set(tool_gate.ACTING) | set(tool_gate.CLEAN) | set(tool_gate._SOURCES) | set(tool_gate._SAY_HEADS))


def eval_labels(root: Path) -> frozenset[str]:
    """Every label the intent eval knows (its LABELS, TOOL_LABELS and CODE_ACTING), read from
    bridge/tools/intent_eval.py without importing it; empty when the file is not there."""
    import ast

    path = root / "bridge" / "tools" / "intent_eval.py"
    if not path.is_file():
        return frozenset()
    out: set[str] = set()
    for node in ast.parse(path.read_text(encoding="utf-8")).body:
        target = node.target if isinstance(node, ast.AnnAssign) else (node.targets[0] if isinstance(node, ast.Assign)
                                                                       else None)
        if isinstance(target, ast.Name) and target.id in ("LABELS", "TOOL_LABELS", "CODE_ACTING", "NONE", "ASK"):
            value = node.value
            if isinstance(value, ast.Dict):                     # TOOL_LABELS: its labels are the values
                value = ast.Tuple(elts=[v for v in value.values if v is not None], ctx=ast.Load())
            out |= {c.value for c in ast.walk(value) if isinstance(c, ast.Constant) and isinstance(c.value, str)}
    return frozenset(out)


def paths(name: str, root: Path) -> dict[str, Path]:
    return {"module": root / "bridge" / "src" / "eve_bridge" / f"{name}.py",
            "test": root / "bridge" / "tests" / f"test_{name}.py",
            "doc": root / "docs" / f"{name}.md",
            "registry": root / "bridge" / "src" / "eve_bridge" / "app_registry.py"}


def scaffold(name: str, root: Optional[Path] = None, *, dry_run: bool = False) -> list[Path]:
    """Write the app ``name`` under ``root`` (found from the working folder when None); the paths it wrote, or would
    write with ``dry_run``. Raises Refused, with nothing written, for a bad or taken name."""
    root = find_root() if root is None else Path(root)
    problem = app_registry.name_problem(name, own_prefixes(), eval_labels(root) | own_names())
    where = paths(name, root)
    if not problem:
        taken = [p for k, p in where.items() if k != "registry" and p.exists()]
        if taken:
            problem = f"{taken[0].relative_to(root)} already exists"
    registry = where["registry"].read_text(encoding="utf-8") if where["registry"].is_file() else ""
    if not problem and app_registry.MARKER not in registry:
        problem = f"the marker line is missing from {where['registry'].relative_to(root)}"
    if not problem and f'slot="{name}"' in registry:
        problem = f"{name} is already in the registry"
    if problem:
        raise Refused(problem)
    words = {"name": name, "Name": name[:1].upper() + name[1:], "SWITCH": f"EVE_{name.upper()}"}
    out = {
        where["module"]: Template((HERE / "module.py.tmpl").read_text(encoding="utf-8")).substitute(words),
        where["test"]: Template((HERE / "test.py.tmpl").read_text(encoding="utf-8")).substitute(words),
        where["doc"]: Template((HERE / "doc.md.tmpl").read_text(encoding="utf-8")).substitute(words),
    }
    marker_at = registry.index("    " + app_registry.MARKER)
    out[where["registry"]] = registry[:marker_at] + REGISTRY_LINE.substitute(words) + registry[marker_at:]
    if dry_run:
        return list(out)
    for path, text in out.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return list(out)


def cli(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="eve-bridge app", description="eve's apps: the owner's picks, and yours.")
    sub = parser.add_subparsers(dest="cmd")
    p_new = sub.add_parser("new", help="write a new app: a module, its test, its doc and its registry line")
    p_new.add_argument("name", help="2 to 16 lowercase letters or digits, starting with a letter (e.g. fan)")
    p_new.add_argument("--dry-run", action="store_true", help="say what would be written; write nothing")
    sub.add_parser("list", help="every app: its slot, its switch, whether the switch is on here, its doc")
    args = parser.parse_args(argv)
    if args.cmd == "list":
        for app in app_registry.APPS:
            on = "on" if app_registry.switched_on(app) else "off"
            print(f"{app.slot:<10} {app.switch:<14} {on:<4} {app.doc}")
        return 0
    if args.cmd != "new":
        parser.print_help()
        return 0 if not argv else 2
    try:
        root = find_root()
        wrote = scaffold(args.name, root, dry_run=args.dry_run)
    except Refused as e:
        print(f"Not written: {e}.", file=sys.stderr)
        return 1
    verb = "Would write" if args.dry_run else "Wrote"
    for path in wrote:
        print(f"{verb} {path.relative_to(root)}")
    if args.dry_run:
        return 0
    switch = f"EVE_{args.name.upper()}"
    print(f"""
Next:
  1. Run its tests: uv run --project bridge --extra dev pytest -q -p no:cacheprovider -c bridge/pyproject.toml \\
       --rootdir bridge bridge/tests/test_{args.name}.py
  2. Fill in the device code in bridge/src/eve_bridge/{args.name}.py (the comment in its __init__ says where), and
     set symbol and detail in its registry line. Changing its tools? See "Change the tools" in the walk-through.
  3. Run the whole suite and lint (AGENTS.md, "Working on the code"); a new app needs no edit to other tests.
  4. Turn it on, once it is merged into the branch the daemon runs from (the main checkout; AGENTS.md rule 5):
     add {switch}=1 to ~/.config/eve-bridge/env, then restart the daemon:
       launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
  5. If it will stay on, pre-register a bar and run a blind intent_eval set first (AGENTS.md rule 6).
See docs/adding-an-app.md.""")
    return 0
