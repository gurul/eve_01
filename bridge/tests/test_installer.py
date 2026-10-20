"""Installer tests — run against a temp settings.json so we never touch the real one."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from eve_bridge import installer


@pytest.fixture
def temp_settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point $CLAUDE_CONFIG_DIR at a temp home.

    Driving the real env var rather than patching a module constant is
    deliberate: resolving the config home *is* the thing that regressed in
    production (hooks silently installed into a home no session reads), so the
    tests exercise that resolution instead of bypassing it.
    """
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path))
    return tmp_path / "settings.json"


def _baseline(extra: dict | None = None) -> dict:
    d = {
        "statusLine": {"type": "command", "command": "true"},
        "permissions": {"defaultMode": "auto"},
    }
    if extra:
        d.update(extra)
    return d


def _write(p: Path, data: dict) -> None:
    p.write_text(json.dumps(data, indent=2), encoding="utf-8")


def test_install_from_scratch(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    assert installer.install_hooks() == 0
    data = json.loads(temp_settings.read_text())
    assert "hooks" in data
    # All 7 hook events covered.
    assert set(data["hooks"].keys()) == {
        "PreToolUse", "PostToolUse", "SessionStart", "SessionEnd",
        "UserPromptSubmit", "Stop", "Notification", "PermissionRequest",
    }
    # Non-hook settings preserved.
    assert data["statusLine"]["command"] == "true"
    assert data["permissions"]["defaultMode"] == "auto"


def test_install_is_idempotent(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    installer.install_hooks()
    first = json.loads(temp_settings.read_text())
    installer.install_hooks()
    second = json.loads(temp_settings.read_text())
    # Same number of entries — no duplicates.
    assert len(first["hooks"]["PreToolUse"][0]["hooks"]) == len(second["hooks"]["PreToolUse"][0]["hooks"]) == 1


def test_uninstall_removes_only_our_entries(temp_settings: Path) -> None:
    baseline = _baseline({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {"type": "command", "command": "/path/to/unrelated-hook", "timeout": 5},
                    ],
                }
            ]
        }
    })
    _write(temp_settings, baseline)
    installer.install_hooks()

    # Now install added our Bash hook alongside the user's.
    after_install = json.loads(temp_settings.read_text())
    bash_group = after_install["hooks"]["PreToolUse"][0]
    assert bash_group["matcher"] == "Bash"
    assert len(bash_group["hooks"]) == 2

    assert installer.uninstall_hooks() == 0
    after_uninstall = json.loads(temp_settings.read_text())
    # User's unrelated hook survived.
    bash_group = after_uninstall["hooks"]["PreToolUse"][0]
    assert len(bash_group["hooks"]) == 1
    assert bash_group["hooks"][0]["command"] == "/path/to/unrelated-hook"


def test_uninstall_drops_empty_hooks_block(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    installer.install_hooks()
    installer.uninstall_hooks()
    data = json.loads(temp_settings.read_text())
    # Nothing else was in hooks; block should be gone.
    assert "hooks" not in data


def test_uninstall_when_nothing_to_remove(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    assert installer.uninstall_hooks() == 0  # no-op, exit clean


def test_install_replaces_the_hooks_from_before_the_rename(temp_settings: Path) -> None:
    # 2026-10-10: after the rename the old entries stayed, every hook failed, and "claude on" found no session.
    old = "/old/venv/bin/python -m cc_bu" + "ddy_bridge.hooks."
    foreign = {"type": "command", "command": "other-tool hook"}
    _write(temp_settings, _baseline({"hooks": {
        "SessionStart": [{"hooks": [{"type": "command", "command": old + "session_start"}, foreign]}],
        "Stop": [{"hooks": [{"type": "command", "command": old + "stop"}]}],
    }}))
    assert installer.install_hooks() == 0
    text = temp_settings.read_text()
    assert "ddy_bridge" not in text and "other-tool hook" in text
    data = json.loads(text)
    starts = [h["command"] for g in data["hooks"]["SessionStart"] for h in g["hooks"]]
    assert any(c.endswith("-m eve_bridge.hooks.session_start") for c in starts)
    assert all(g["hooks"] for e in data["hooks"].values() for g in e)          # no empty groups left behind


def test_hooks_run_isolated_so_the_session_folder_is_not_on_the_import_path(tmp_path, monkeypatch):
    """A hook runs in the session's folder; with plain ``-m`` a planted json.py there ran instead of the stdlib's."""
    import subprocess
    import sys

    from eve_bridge import installer
    cmd = installer._hook_command("eve_bridge.hooks.stop")
    assert " -I -m eve_bridge.hooks.stop" in cmd
    (tmp_path / "json.py").write_text("print('HIJACKED')\n")
    probe = "import json, sys; print('ok' if 'HIJACKED' not in open(json.__file__).read() else 'bad')"
    isolated = subprocess.run([sys.executable, "-I", "-c", probe], cwd=tmp_path, capture_output=True, text=True)
    assert isolated.stdout.strip() == "ok"
    plain = subprocess.run([sys.executable, "-m", "json.tool", "--help"], cwd=tmp_path, capture_output=True, text=True)
    assert "HIJACKED" in plain.stdout                                  # positive control: the hole is real without -I


# ---- the Rust hook binaries (--hooks-dir) ----

def _fake_bins(d: Path, skip: str | None = None) -> Path:
    d.mkdir(parents=True, exist_ok=True)
    for _, module, _, _ in installer.HOOK_DEFS:
        name = installer.native_binary(module)
        if name == skip:
            continue
        b = d / name
        b.write_text("#!/bin/sh\nexit 0\n")
        b.chmod(0o755)
    return d


def test_native_binary_names_follow_the_rust_crate():
    assert installer.native_binary("eve_bridge.hooks.session_start") == "eve-hook-session-start"
    assert installer.native_binary("eve_bridge.hooks.pretooluse") == "eve-hook-pretooluse"
    assert installer.native_binary("eve_bridge.hooks.permission_request") == "eve-hook-permission-request"


def test_install_with_hooks_dir_registers_every_rust_binary(temp_settings: Path, tmp_path: Path) -> None:
    _write(temp_settings, _baseline())
    bins = _fake_bins(tmp_path / "release dir")
    assert installer.install_hooks(hooks_dir=str(bins)) == 0
    data = json.loads(temp_settings.read_text())
    cmds = [h["command"] for groups in data["hooks"].values() for g in groups for h in g["hooks"]]
    assert len(cmds) == len(installer.HOOK_DEFS)
    for _, module, _, _ in installer.HOOK_DEFS:
        expected = f"'{bins.resolve() / installer.native_binary(module)}' # {module}"
        assert expected in cmds
    assert not any("-m eve_bridge" in c for c in cmds)


def test_install_with_hooks_dir_refuses_a_missing_binary(temp_settings: Path, tmp_path: Path, capsys) -> None:
    _write(temp_settings, _baseline())
    bins = _fake_bins(tmp_path / "bins", skip="eve-hook-stop")
    assert installer.install_hooks(hooks_dir=str(bins)) == 2
    assert "eve-hook-stop" in capsys.readouterr().err
    assert "hooks" not in json.loads(temp_settings.read_text())


def test_python_and_rust_hooks_replace_each_other(temp_settings: Path, tmp_path: Path) -> None:
    _write(temp_settings, _baseline())
    bins = _fake_bins(tmp_path / "bins")

    def ours() -> list[str]:
        data = json.loads(temp_settings.read_text())
        return [h["command"] for groups in data.get("hooks", {}).values() for g in groups for h in g["hooks"]
                if installer._is_our_entry(h)]

    installer.install_hooks()
    assert len(ours()) == len(installer.HOOK_DEFS) and all("-m eve_bridge" in c for c in ours())
    installer.install_hooks(hooks_dir=str(bins))
    assert len(ours()) == len(installer.HOOK_DEFS) and all("eve-hook-" in c for c in ours())
    installer.install_hooks()
    assert len(ours()) == len(installer.HOOK_DEFS) and all("-m eve_bridge" in c for c in ours())
    installer.install_hooks(hooks_dir=str(bins))
    assert installer.uninstall_hooks() == 0
    assert ours() == []


def test_a_registered_rust_hook_reaches_the_daemon_socket(temp_settings: Path, tmp_path: Path) -> None:
    """The real binary, run the way Claude Code runs the registered command, posts its event to the socket."""
    import shutil
    import socket
    import subprocess
    import threading
    release = Path(__file__).resolve().parents[2] / "rust" / "target" / "release"
    if not (release / "eve-hook-posttooluse").exists() or shutil.which("sh") is None:
        pytest.skip("rust hooks not built (cd rust && cargo build --release -p eve-hooks)")
    _write(temp_settings, _baseline())
    assert installer.install_hooks(hooks_dir=str(release)) == 0
    cmd = next(h["command"] for g in json.loads(temp_settings.read_text())["hooks"]["PostToolUse"]
               for h in g["hooks"] if installer._is_our_entry(h))
    sock_path = Path("/tmp") / f"eve-test-{tmp_path.name[-12:]}.sock"
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(str(sock_path))
    srv.listen(1)
    got: list[bytes] = []

    def serve() -> None:
        conn, _ = srv.accept()
        with conn:
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                buf += chunk
            got.append(buf)
            conn.sendall(b'{"ok": true}\n')

    t = threading.Thread(target=serve, daemon=True)
    t.start()
    try:
        payload = json.dumps({"session_id": "s-test", "hook_event_name": "PostToolUse", "tool_name": "Read",
                              "tool_input": {}, "tool_response": {}})
        env = {"PATH": "/usr/bin:/bin", "HOME": str(tmp_path), "EVE_BRIDGE_SOCK": str(sock_path)}
        p = subprocess.run(["sh", "-c", cmd], input=payload.encode(), capture_output=True, env=env, timeout=10)
        t.join(5)
        assert p.returncode == 0, p.stderr
        assert got and json.loads(got[0])["evt"] == "posttooluse"
    finally:
        srv.close()
        sock_path.unlink(missing_ok=True)
