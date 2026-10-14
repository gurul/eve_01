"""eve lite's install closure (WP-12): every package the release installs on an Apple-silicon Mac has a wheel for
uv's CPython there, so install-lite.sh's `uv pip install --only-binary :all: --require-hashes` never needs a compiler.

The closure is worked out from uv.lock (the core dependencies plus the `lite` extra) for install-lite.sh's own pins:
its PYTHON_VERSION, macOS MACOS_MIN and arm64. It is checked against the file the release ships,
`tools/release_lite.sh --requirements`. Controls: without the release's exclusions the same scan reports the
pyautogui family, which PyPI has only sdists for; an Intel or a macOS 13 target reports the packages whose macOS
wheels are arm64-only or need macOS 14."""

from __future__ import annotations

import re
import shutil
import subprocess
import tomllib
from pathlib import Path
from urllib.parse import unquote

import pytest
from packaging import tags
from packaging.markers import Marker
from packaging.utils import parse_wheel_filename

BRIDGE = Path(__file__).resolve().parents[1]
ROOT = BRIDGE.parent
INSTALLER = ROOT / "install-lite.sh"
RELEASE = ROOT / "tools" / "release_lite.sh"
LOCK = tomllib.loads((BRIDGE / "uv.lock").read_text())
PYPROJECT = tomllib.loads((BRIDGE / "pyproject.toml").read_text())

# uv.lock has only sdists for these six (2026-10-13). pymsgbox has a wheel, but only pyautogui needs it.
NO_WHEEL = {"pyautogui", "mouseinfo", "pygetwindow", "pyrect", "pyscreeze", "pytweening"}


def _pin(name: str) -> str:
    found = re.findall(rf'^{name}="?([^"\n]*)"?$', INSTALLER.read_text(), re.M)
    assert len(found) == 1, f"install-lite.sh must set {name} exactly once"
    return found[0]


def _release_exclusions() -> set[str]:
    found = re.findall(r'^NO_WHEEL="([^"]*)"$', RELEASE.read_text(), re.M)
    assert len(found) == 1, "tools/release_lite.sh must set NO_WHEEL exactly once"
    return set(found[0].split())


PYTHON = _pin("PYTHON_VERSION")
MACOS = int(_pin("MACOS_MIN"))


def _target(python: str = PYTHON, macos: int = MACOS, machine: str = "arm64") -> dict[str, str]:
    """PEP 508 marker values on the Mac the installer targets (Darwin 23 is macOS 14)."""
    major, minor = python.split(".")[:2]
    return {
        "implementation_name": "cpython",
        "implementation_version": python,
        "os_name": "posix",
        "platform_machine": machine,
        "platform_python_implementation": "CPython",
        "platform_release": f"{macos + 9}.0.0",
        "platform_system": "Darwin",
        "platform_version": f"Darwin Kernel Version {macos + 9}.0.0",
        "python_full_version": python,
        "python_version": f"{major}.{minor}",
        "sys_platform": "darwin",
        "extra": "",
    }


def _supported(python: str = PYTHON, macos: int = MACOS, machine: str = "arm64") -> set[tags.Tag]:
    """Every wheel tag uv's CPython accepts there: cp313 and abi3, pure, arm64 and universal2 up to that macOS."""
    version = tuple(int(x) for x in python.split(".")[:2])
    interpreter = f"cp{version[0]}{version[1]}"
    platforms = list(tags.mac_platforms((macos, 0), machine))
    return (set(tags.cpython_tags(version, [interpreter], platforms))
            | set(tags.compatible_tags(version, interpreter, platforms)))


def lite_closure(env: dict[str, str]) -> dict[str, dict]:
    """name -> the uv.lock entry an install of eve-bridge[lite] uses on a machine with these marker values."""
    by_name: dict[str, list[dict]] = {}
    for pkg in LOCK["package"]:
        by_name.setdefault(pkg["name"], []).append(pkg)

    def pick(dep: dict) -> dict:
        found = [p for p in by_name[dep["name"]] if "version" not in dep or p["version"] == dep["version"]]
        assert len(found) == 1, f"uv.lock edge {dep} is ambiguous"
        return found[0]

    root = pick({"name": "eve-bridge"})
    closure: dict[str, dict] = {}
    seen: set[tuple[str, str, str]] = set()
    todo: list[tuple[dict, str]] = [(root, ""), (root, "lite")]
    while todo:
        pkg, extra = todo.pop()
        if (pkg["name"], pkg["version"], extra) in seen:
            continue
        seen.add((pkg["name"], pkg["version"], extra))
        if pkg is not root:
            assert closure.setdefault(pkg["name"], pkg) is pkg, f"two versions of {pkg['name']} in one closure"
        deps = pkg.get("optional-dependencies", {}).get(extra, []) if extra else pkg.get("dependencies", [])
        for dep in deps:
            if "marker" in dep and not Marker(dep["marker"]).evaluate(env):
                continue
            child = pick(dep)
            todo.append((child, ""))
            todo.extend((child, e) for e in dep.get("extra", []))
    return closure


def wheel_names(pkg: dict) -> list[str]:
    return [unquote(w["url"].rsplit("/", 1)[-1]) for w in pkg.get("wheels", [])]


def without_wheel(closure: dict[str, dict], supported: set[tags.Tag], exclude: set[str]) -> set[str]:
    """The packages in the closure, apart from EXCLUDE, that have no wheel with a supported tag."""
    return {name for name, pkg in closure.items() if name not in exclude
            and not any(set(parse_wheel_filename(f)[3]) & supported for f in wheel_names(pkg))}


def test_every_lite_package_has_a_wheel_for_the_installers_python_and_macos() -> None:
    closure = lite_closure(_target())
    assert len(closure) > 50, "the closure should hold the core dependencies and the lite extra"
    assert {"segno", "mem0ai", "aiortc", "av", "cryptography"} <= closure.keys()
    assert without_wheel(closure, _supported(), _release_exclusions()) == set()


def test_positive_control_without_the_exclusions_the_scan_reports_the_pyautogui_family() -> None:
    assert without_wheel(lite_closure(_target()), _supported(), set()) == NO_WHEEL


def test_positive_control_an_intel_or_macos_13_target_is_reported() -> None:
    """The tag check is not vacuous: cryptography's macOS wheels are arm64 only, and av's arm64 wheel needs macOS 14."""
    exclude = _release_exclusions()
    intel = without_wheel(lite_closure(_target(machine="x86_64")), _supported(machine="x86_64"), exclude)
    assert "cryptography" in intel
    assert "av" in without_wheel(lite_closure(_target(macos=13)), _supported(macos=13), exclude)


def test_release_excludes_the_family_and_nothing_else() -> None:
    exclude = _release_exclusions()
    assert exclude == NO_WHEEL | {"pymsgbox"}
    # pymsgbox goes with them only because nothing outside the family needs it
    needs_msgbox = {p["name"] for p in LOCK["package"]
                    if any(d["name"] == "pymsgbox" for d in p.get("dependencies", []))}
    assert needs_msgbox <= exclude


def test_lite_extra_is_declared_locked_and_ships_the_web_page() -> None:
    assert PYPROJECT["project"]["optional-dependencies"]["lite"] == ["mem0ai>=2.2,<3", "segno>=1.6,<2"]
    me = next(p for p in LOCK["package"] if p["name"] == "eve-bridge")
    assert {d["name"] for d in me["optional-dependencies"]["lite"]} == {"mem0ai", "segno"}
    assert "lite" in me["metadata"]["provides-extras"]
    data = PYPROJECT["tool"]["setuptools"]["package-data"]["eve_bridge"]
    assert {"web_lite/*", "web_lite/**/*"} <= set(data)


def _uv() -> str | None:
    for candidate in (shutil.which("uv"), "/opt/homebrew/bin/uv", "/usr/local/bin/uv"):
        if candidate and Path(candidate).is_file():
            return candidate
    return None


def _parse_requirements(text: str) -> list[tuple[str, str, str, list[str]]]:
    """requirements.txt blocks from `uv export` -> (name, version, marker, sha256 hashes)."""
    blocks: list[tuple[str, str, str, list[str]]] = []
    for chunk in re.split(r"\n(?=[A-Za-z0-9])", text):
        head = re.match(r"([A-Za-z0-9][A-Za-z0-9._-]*)==([^\s;\\]+)(?:\s*;\s*([^\\\n]+))?", chunk)
        if head:
            name, version, marker = head.group(1), head.group(2), (head.group(3) or "").strip()
            blocks.append((name, version, marker, re.findall(r"--hash=sha256:([0-9a-f]{64})", chunk)))
    return blocks


def test_release_requirements_are_the_closure_hash_pinned_to_installable_wheels(tmp_path: Path) -> None:
    uv = _uv()
    if uv is None:
        pytest.skip("uv is not installed here")
    out = tmp_path / "requirements-lite.txt"
    env = {"HOME": str(tmp_path), "TMPDIR": str(tmp_path), "PATH": f"{Path(uv).parent}:/usr/bin:/bin:/usr/sbin:/sbin"}
    done = subprocess.run(["sh", str(RELEASE), "--requirements", str(out)], env=env, capture_output=True, text=True,
                          timeout=120)
    assert done.returncode == 0, done.stderr
    text = out.read_text()
    blocks = _parse_requirements(text)
    assert len(blocks) == len(re.findall(r"^[A-Za-z0-9][A-Za-z0-9._-]*==", text, re.M)) > 50
    assert all(hashes for *_, hashes in blocks), "every requirement carries --hash"
    assert not re.search(r"^(-e|\.|eve-bridge)", text, re.M), "the project itself is installed from the wheel"
    assert not {name for name, *_ in blocks} & _release_exclusions()

    env_target = _target()
    shipped = {(name, version): hashes for name, version, marker, hashes in blocks
               if not marker or Marker(marker).evaluate(env_target)}
    closure = lite_closure(env_target)
    expected = {(name, pkg["version"]) for name, pkg in closure.items() if name not in _release_exclusions()}
    assert set(shipped) == expected

    # --only-binary with --require-hashes: each shipped package lists the hash of a wheel this Mac can install
    supported = _supported()
    for (name, _version), hashes in shipped.items():
        good = {w["hash"].removeprefix("sha256:") for w in closure[name].get("wheels", [])
                if set(parse_wheel_filename(unquote(w["url"].rsplit("/", 1)[-1]))[3]) & supported}
        assert good & set(hashes), f"{name}: no hash of an installable wheel"
