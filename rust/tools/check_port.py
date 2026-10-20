#!/usr/bin/env python3
"""Decide whether crates in plan.json are ported, against the Python source.

For each crate it checks that the crate directory, Cargo.toml (with the right
package name), a lib or main target and PARITY.md exist; that PARITY.md says
`Status: complete` and names every top-level def/class of each Python module;
and that no placeholder (todo!, unimplemented!, "not implemented", "TODO: port")
appears in the crate's Rust sources.

    python3 tools/check_port.py                 # every crate
    python3 tools/check_port.py --crate web     # one crate
    python3 tools/check_port.py --self-test     # proves the checker can fail

Prints "port check passed" only when every selected crate passes.
"""
from __future__ import annotations

import ast
import json
import re
import sys
import tempfile
from pathlib import Path

RUST = Path(__file__).resolve().parent.parent
PY_SRC = RUST.parent / "bridge" / "src"
PLACEHOLDER = re.compile(
    r"\btodo!\s*\(|\bunimplemented!\s*\(|not (yet )?implemented|TODO:? *port|\bstub(bed)? out\b",
    re.IGNORECASE,
)


def python_path(module: str) -> Path:
    if module == "eve_bridge":
        return PY_SRC / "eve_bridge" / "__init__.py"
    rel = PY_SRC / "eve_bridge" / Path(*module.split("."))
    pkg = rel / "__init__.py"
    return pkg if pkg.exists() else rel.with_suffix(".py")


def top_level_names(path: Path) -> list[str]:
    tree = ast.parse(path.read_text())
    return [
        n.name
        for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
    ]


def check_crate(crate: dict, root: Path) -> list[str]:
    problems: list[str] = []
    name = crate["name"]
    d = root / "crates" / f"eve-{name}"
    if not d.is_dir():
        return [f"{name}: crate directory {d.relative_to(root)} missing"]
    manifest = d / "Cargo.toml"
    if not manifest.exists():
        problems.append(f"{name}: Cargo.toml missing")
    elif not re.search(rf'^name\s*=\s*"eve-{re.escape(name)}"', manifest.read_text(), re.M):
        problems.append(f"{name}: Cargo.toml package name is not eve-{name}")
    if not ((d / "src" / "lib.rs").exists() or (d / "src" / "main.rs").exists()):
        problems.append(f"{name}: no src/lib.rs or src/main.rs")
    parity = d / "PARITY.md"
    if not parity.exists():
        problems.append(f"{name}: PARITY.md missing")
        text = ""
    else:
        text = parity.read_text()
        if not re.search(r"^Status:\s*complete\s*$", text, re.M):
            problems.append(f"{name}: PARITY.md is not 'Status: complete'")
    for module in crate["modules"]:
        for fn in top_level_names(python_path(module)):
            if not re.search(rf"\b{re.escape(fn)}\b", text):
                problems.append(f"{name}: PARITY.md has no row for {module}.{fn}")
    for rs in sorted(d.rglob("*.rs")):
        if "target" in rs.parts:
            continue
        for i, line in enumerate(rs.read_text().splitlines(), 1):
            if PLACEHOLDER.search(line):
                problems.append(f"{name}: placeholder at {rs.relative_to(root)}:{i}: {line.strip()[:80]}")
    return problems


def self_test() -> int:
    """Run the checker on a fixture crate that must fail and one that must pass."""
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        crate = {"name": "consent", "modules": ["consent"]}
        d = root / "crates" / "eve-consent" / "src"
        d.mkdir(parents=True)
        (d / "lib.rs").write_text("pub fn f() { todo!() }\n")
        (d.parent / "Cargo.toml").write_text('[package]\nname = "eve-consent"\n')
        (d.parent / "PARITY.md").write_text("Status: partial\n")
        bad = check_crate(crate, root)
        if not bad:
            print("self-test FAILED: a placeholder crate passed")
            return 1
        names = " ".join(top_level_names(python_path("consent")))
        (d / "lib.rs").write_text("pub fn f() {}\n")
        (d.parent / "PARITY.md").write_text(f"Status: complete\n{names}\n")
        good = check_crate(crate, root)
        if good:
            print("self-test FAILED: a complete crate failed:", good)
            return 1
    print("self-test passed")
    return 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()
    plan = json.loads((RUST / "plan.json").read_text())
    if "--crate" in argv:
        want = argv[argv.index("--crate") + 1]
        plan = [c for c in plan if c["name"] == want]
        if not plan:
            print(f"no crate {want} in plan.json")
            return 2
    problems = [p for c in plan for p in check_crate(c, RUST)]
    ok = len(plan) - len({p.split(":")[0] for p in problems})
    for p in problems:
        print(p)
    print(f"{ok}/{len(plan)} crates pass")
    if problems:
        return 1
    print("port check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
