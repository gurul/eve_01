"""profile.LITE_CLASSIFIED: every EVE_* switch a module of eve lite's daemon reads has a recorded reason to be forced or
left alone, and every secret those modules read is removed by profile.apply (docs/plans/eve-lite.md WP-2).

The module set is worked out here, from the source: the lite composition (lite.py and the parts it builds, M1) and
everything their imports reach, function-level imports included (a lazy import runs the day its function does), plus
what importing the CLI loads (the daemon starts as `eve_bridge.cli daemon`; its other commands run in other processes).
A new switch, or a new secret, in any of those modules fails here until someone decides what eve lite does with it.
"""

from __future__ import annotations

import ast
import re
from functools import lru_cache
from pathlib import Path
from typing import Mapping, Optional

from eve_bridge import profile

PKG = Path(__file__).resolve().parents[1] / "src" / "eve_bridge"
CANARY = "CANARY-NOT-A-KEY-7f3c"

# The lite composition (M1): what lite.py builds, and lite's own modules (WP-4 to WP-9), each counted once it exists.
COMPOSITION = ("telegram", "link_chat", "phone_call", "watch", "memory", "chat_log", "recall", "think", "spend_sync",
               "ipc", "envfile", "records", "dream", "profile", "toolchain")
LITE_MODULES = ("lite", "lite_web", "lite_rtc", "lite_tls", "lan_net", "web_auth", "onboarding", "composio_connect",
                "composio_login", "composio_platform_lite")
SWITCH = re.compile(r"^EVE_[A-Z0-9_*]+$")
ENV_NAME = re.compile(r"^[A-Z][A-Z0-9_]*[A-Z0-9]$")
# A name that mentions one of these may hold a secret: apply removes it (profile.is_secret_name), or
# profile.NOT_SECRET_NAMES says what it holds instead.
SECRETISH = re.compile(r"KEY|TOKEN|SECRET|PASSW|CREDENTIAL|AUTH|COOKIE|PRIVATE")


def all_modules() -> dict[str, Path]:
    found: dict[str, Path] = {}
    for path in PKG.rglob("*.py"):
        parts = list(path.relative_to(PKG).with_suffix("").parts)
        if parts[-1] == "__init__":
            parts = parts[:-1]
        found[".".join(["eve_bridge", *parts])] = path
    return found


@lru_cache(maxsize=None)
def _tree(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"))


def imports_of(name: str, path: Path, modules: Mapping[str, Path], top_only: bool) -> set[str]:
    """The eve_bridge modules ``name`` imports: at module level only, or anywhere in it (functions included). A
    submodule brings its packages' __init__ along; importlib.import_module with a fixed name counts too."""
    package = name.split(".") if path.name == "__init__.py" else name.split(".")[:-1]
    wanted: set[str] = set()

    def visit(node: ast.AST, nested: bool) -> None:
        for child in ast.iter_child_nodes(node):
            inside = nested or isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda))
            if top_only and inside:
                continue
            if isinstance(child, ast.Import):
                wanted.update(a.name for a in child.names)
            elif isinstance(child, ast.ImportFrom):
                base = (".".join(package[:len(package) - child.level + 1] + ([child.module] if child.module else []))
                        if child.level else (child.module or ""))
                wanted.add(base)
                wanted.update(f"{base}.{a.name}" for a in child.names)
            elif (isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute)
                  and child.func.attr == "import_module" and child.args
                  and isinstance(child.args[0], ast.Constant) and isinstance(child.args[0].value, str)):
                wanted.add(child.args[0].value)
            visit(child, inside)

    visit(_tree(path), False)
    found: set[str] = set()
    for module in wanted:
        while module and module not in modules:            # "eve_bridge.x.attr" is the module eve_bridge.x
            module = module.rpartition(".")[0]
        if module.startswith("eve_bridge"):
            parts = module.split(".")
            found.update(".".join(parts[:i]) for i in range(1, len(parts) + 1) if ".".join(parts[:i]) in modules)
    return found


def closure(roots: list[str], modules: Mapping[str, Path], top_only: bool) -> set[str]:
    seen: set[str] = set()
    todo = [r for r in roots if r in modules]
    while todo:
        module = todo.pop()
        if module not in seen:
            seen.add(module)
            todo.extend(imports_of(module, modules[module], modules, top_only) - seen)
    return seen


def lite_module_set(modules: Optional[Mapping[str, Path]] = None) -> set[str]:
    modules = all_modules() if modules is None else modules
    roots = [f"eve_bridge.{m}" for m in COMPOSITION + LITE_MODULES]
    return closure(roots, modules, top_only=False) | closure(["eve_bridge.cli"], modules, top_only=True)


def _pattern(node: ast.JoinedStr) -> str:
    """An f-string's text with each value written as *: f"EVE_{name.upper()}_FILE" is EVE_*_FILE."""
    return "".join(v.value if isinstance(v, ast.Constant) and isinstance(v.value, str) else "*" for v in node.values)


def switches_in(tree: ast.AST) -> set[str]:
    """Every EVE_* name in the source: a string that is one, or an f-string that builds one."""
    found: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and SWITCH.match(node.value):
            found.add(node.value)
        elif isinstance(node, ast.JoinedStr) and SWITCH.match(_pattern(node)) and _pattern(node).startswith("EVE_"):
            found.add(_pattern(node))
    return found


def names_in(tree: ast.AST) -> set[str]:
    """Every string in the source shaped like an environment variable's name."""
    return {n.value for n in ast.walk(tree) if isinstance(n, ast.Constant) and isinstance(n.value, str)
            and ENV_NAME.match(n.value)}


def unclassified(sources: Mapping[str, ast.AST]) -> dict[str, list[str]]:
    """Each EVE_* name these modules read that LITE_CLASSIFIED has no reason for, with the modules that read it."""
    missing: dict[str, list[str]] = {}
    for module, tree in sorted(sources.items()):
        for name in sorted(switches_in(tree) - set(profile.LITE_CLASSIFIED)):
            missing.setdefault(name, []).append(module)
    return missing


def lite_sources() -> dict[str, ast.AST]:
    """The modules of the set as parsed source, profile.py left out: its names are the registry itself, not reads."""
    modules = all_modules()
    return {m: _tree(modules[m]) for m in lite_module_set(modules) if m != "eve_bridge.profile"}


# ---- the switches ---------------------------------------------------------------------------------------------------

def test_every_switch_a_lite_module_reads_is_classified() -> None:
    missing = unclassified(lite_sources())
    assert missing == {}, ("give each name a reason in profile.LITE_CLASSIFIED, or force it in LITE_FORCED: "
                           + "; ".join(f"{n} ({', '.join(m)})" for n, m in missing.items()))


def test_the_module_set_comes_from_the_imports() -> None:
    """Controls: the composition, what it imports at module level, and what a function imports later are in;
    the code that runs in other processes (Claude Code's hooks, the Work tab's MCP server, the installer) is out."""
    found = lite_module_set()
    assert {"eve_bridge.telegram", "eve_bridge.records", "eve_bridge.profile", "eve_bridge.toolchain",
            "eve_bridge.websearch", "eve_bridge.app_registry", "eve_bridge.cli"} <= found
    assert {"eve_bridge.desk_lane", "eve_bridge.mem0_memory", "eve_bridge.web_reader"} <= found   # imported lazily
    assert not {"eve_bridge.hooks.pretooluse", "eve_bridge.eve_mcp", "eve_bridge.installer"} & found
    modules = all_modules()
    telegram_top = imports_of("eve_bridge.telegram", modules["eve_bridge.telegram"], modules, top_only=True)
    assert "eve_bridge.desk_lane" not in telegram_top and "eve_bridge.websearch" in telegram_top
    for lite_module in LITE_MODULES:                                  # each lite module once it exists
        if f"eve_bridge.{lite_module}" in modules:
            assert f"eve_bridge.{lite_module}" in found


def test_the_scanner_catches_an_unclassified_switch() -> None:
    """Positive control: a module reading a switch nobody classified, plain or built at run time, is reported."""
    fake = ast.parse('import os\n'
                     'A = os.environ.get("EVE_CANARY_NOT_CLASSIFIED")\n'
                     'B = os.environ.get(f"EVE_{name.upper()}_CANARY")\n'
                     'C = os.environ.get("EVE_WATCH")\n'
                     'D = "EVE_WATCH is on unless 0"\n')
    assert unclassified({"eve_bridge.fake": fake}) == {"EVE_CANARY_NOT_CLASSIFIED": ["eve_bridge.fake"],
                                                       "EVE_*_CANARY": ["eve_bridge.fake"]}
    modules = all_modules()
    assert "EVE_TELEGRAM" in switches_in(_tree(modules["eve_bridge.telegram"]))
    assert "EVE_DESK_LANE" in switches_in(_tree(modules["eve_bridge.desk_lane"]))


def test_each_reason_says_what_it_is_and_holds() -> None:
    forced, defaults = profile.LITE_FORCED, profile.LITE_DEFAULTS
    kinds = ("forced to ", "default when unset: ", "secret-shaped: ", "inert while ", "kept: ", "not run in lite: ",
             "the edition switch")
    for name, reason in profile.LITE_CLASSIFIED.items():
        assert SWITCH.match(name), name
        assert reason.startswith(kinds), (name, reason)
        assert (name in forced) == reason.startswith("forced to "), name
        if name in forced:
            assert reason == f"forced to {forced[name]}", name
        assert (name in defaults) == reason.startswith("default when unset: "), name
        secret = profile.is_secret_name(name) and name not in forced and name not in defaults
        assert secret == reason.startswith("secret-shaped: "), name
        if reason.startswith("inert while "):
            named = re.findall(r"EVE_[A-Z0-9_]+", reason.split(":", 1)[0])
            assert named and all(n in forced for n in named), (name, named)
    assert profile.LITE_CLASSIFIED[profile.PROFILE_ENV].startswith("the edition switch")
    assert set(forced) <= set(profile.LITE_CLASSIFIED) and set(defaults) <= set(profile.LITE_CLASSIFIED)


def test_the_reason_check_catches_a_reason_that_does_not_hold() -> None:
    """Positive control: an inert reason naming a switch lite does not force fails the rule above."""
    reason = "inert while EVE_WATCH is forced off: something"
    named = re.findall(r"EVE_[A-Z0-9_]+", reason.split(":", 1)[0])
    assert named == ["EVE_WATCH"] and not all(n in profile.LITE_FORCED for n in named)


# ---- the secrets ----------------------------------------------------------------------------------------------------

def test_every_secret_a_lite_module_reads_is_removed() -> None:
    names = set().union(*(names_in(tree) for tree in lite_sources().values()))
    secretish = {n for n in names if SECRETISH.search(n)}
    assert {"OPENROUTER_API_KEY", "COMPOSIO_API_KEY", "TINYFISH_API_KEY", "EVE_TELEGRAM_TOKEN"} <= secretish   # control
    not_secrets = profile.NOT_SECRET_NAMES
    undecided = sorted(n for n in secretish if not profile.is_secret_name(n) and n not in not_secrets
                       and n not in profile.LITE_FORCED)
    assert undecided == [], ("a secret goes in profile.SECRET_NAMES (apply removes it); a name that holds none goes "
                             "in profile.NOT_SECRET_NAMES with what it holds: " + ", ".join(undecided))
    env = {"EVE_PROFILE": "lite", "HOME": "/Users/someone", **dict.fromkeys(secretish, CANARY)}
    profile.apply(env)
    left = {n for n, v in env.items() if v == CANARY}
    assert left == set(profile.LITE_SECRET_ALLOW) | (set(not_secrets) & secretish), sorted(left)


def test_the_secret_check_catches_a_secret_of_another_shape() -> None:
    """Positive control: a name like FOO_APIKEY mentions a key, is not secret-shaped and is listed nowhere."""
    name = "FOO_APIKEY"
    assert SECRETISH.search(name) and not profile.is_secret_name(name) and name not in profile.NOT_SECRET_NAMES
