"""Optional, private API-key identity for Eve's Codex children.

The default environment is deliberately identical to the plan-backed adapter.
Only the explicit setup command copies runtime configuration or performs login.
"""
from __future__ import annotations

import json
import logging
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
from collections.abc import Mapping
from pathlib import Path

log = logging.getLogger(__name__)
ENV_KEYS = ('HOME', 'PATH', 'USER', 'LOGNAME', 'TMPDIR', 'LANG', 'CODEX_HOME')
# Inspected in the desktop distribution: config selects marketplaces/plugins;
# plugins/cache contains manifests and .mcp.json, and .plugin-appserver its runtime.
# The generated bundled marketplace is also referenced by config.toml. No session
# databases, desktop IPC, OAuth credentials or account caches belong in this copy.
COPY_PATHS = ('config.toml', 'computer-use', 'browser', 'plugins',
              '.tmp/bundled-marketplaces', 'chrome-native-hosts-v2.json')
EXCLUDED_NAMES = frozenset({'auth.json', '.credentials.json', '.remote-plugin-install-staging'})
_warned: set[str] = set()


class SetupError(Exception):
    """An actionable setup failure, with no subprocess output or credentials."""


def eve_home() -> Path:
    return Path.home() / '.config' / 'eve-bridge' / 'codex-home'


def api_ready(home: Path) -> bool:
    """Require a local API identity, never accept copied ChatGPT credentials."""
    try:
        if (home.is_symlink() or (home / 'auth.json').is_symlink()
                or (home / 'auth.json').stat().st_nlink != 1):
            return False
        data = json.loads((home / 'auth.json').read_text())
        return (isinstance(data, dict) and data.get('auth_mode') == 'apikey'
                and isinstance(data.get('OPENAI_API_KEY'), str)
                and bool(data['OPENAI_API_KEY'].strip()))
    except (OSError, ValueError):
        return False


def _once(reason: str) -> None:
    if reason not in _warned:
        _warned.add(reason)
        log.warning('codex: %s; using the ChatGPT plan. Run eve-bridge codex-home for API setup.', reason)


def launch_environment(environ: Mapping[str, str] | None = None) -> tuple[dict[str, str], str]:
    env = os.environ if environ is None else environ
    child = {k: env[k] for k in ENV_KEYS if k in env}
    mode = env.get('EVE_CODEX_AUTH', 'plan')
    if mode == 'api':
        home = eve_home()
        if api_ready(home):
            child['CODEX_HOME'] = str(home)
            return child, 'api'
        _once('the Eve API home is missing or not signed in with an API key')
    elif mode != 'plan':
        _once('EVE_CODEX_AUTH must be plan or api')
    return child, 'plan'


def _private_directory(path: Path) -> None:
    # Reject symlink ancestors as well as the final path before any write/login.
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise SetupError('Eve Codex home must not be a symlink or have symlink parents.')
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.chmod(0o700)


def _copy_runtime(source: Path, target: Path, source_home: Path, dest_home: Path,
                  ancestors: frozenset[Path] = frozenset()) -> None:
    """Dereference only in-home runtime links; never leave a write-through link."""
    if source.name in EXCLUDED_NAMES:
        return
    real = source.resolve()
    if not real.is_relative_to(source_home) or real.name in EXCLUDED_NAMES:
        raise SetupError('Codex runtime contains a link outside its home or to credentials.')
    if real in ancestors:
        raise SetupError('Codex runtime contains a cyclic directory link.')
    if source.is_dir():
        target.mkdir(parents=True, exist_ok=True)
        for item in source.iterdir():
            _copy_runtime(item, target / item.name, source_home, dest_home, ancestors | {real})
    elif source.is_file():
        personal_auth = source_home / 'auth.json'
        if personal_auth.exists() and source.samefile(personal_auth):
            raise SetupError('Codex runtime contains a hard link to personal credentials.')
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        # Desktop-generated MCP/marketplace files also embed CODEX_HOME. Repoint
        # only our copies. Application binaries and their signatures stay intact.
        if source.suffix in {'.json', '.toml'}:
            text = target.read_text()
            updated = text.replace(str(source_home), str(dest_home))
            if updated != text:
                target.write_text(updated)


def _api_config(path: Path) -> None:
    original = path.read_text() if path.exists() else ''
    config = tomllib.loads(original)
    # Login and runtime must both use the private auth.json, including on machines
    # where the owner's ordinary Codex uses the OS keychain.
    keys = {'cli_auth_credentials_store', 'forced_login_method', 'forced_chatgpt_workspace_id'}
    lines = original.splitlines(keepends=True)
    output: list[str] = []
    in_table = False
    for line in lines:
        if line.lstrip().startswith('['):
            in_table = True
        if not in_table and re.match(r'^\s*(?:' + '|'.join(keys) + r')\s*=', line):
            continue
        output.append(line)
    new = 'cli_auth_credentials_store = "file"\nforced_login_method = "api"\n' + ''.join(output)
    expected = {k: v for k, v in config.items() if k not in keys}
    expected.update(cli_auth_credentials_store='file', forced_login_method='api')
    if tomllib.loads(new) != expected:
        raise SetupError('Could not safely adapt Codex credential storage configuration.')
    path.write_text(new)
    path.chmod(0o600)


def setup(*, binary: str, environ: Mapping[str, str] | None = None) -> Path:
    """Refresh runtime copies and log in once, exclusively inside Eve's home."""
    env = os.environ if environ is None else environ
    source = Path.home() / '.codex'
    destination = eve_home()
    if source.is_symlink() or destination.resolve() == source.resolve():
        raise SetupError('Eve Codex home must be separate from the personal Codex home.')
    _private_directory(destination)
    if (destination / 'auth.json').is_symlink():
        raise SetupError('Eve auth.json must not be a symlink.')
    if (destination / 'auth.json').exists() and (destination / 'auth.json').stat().st_nlink != 1:
        raise SetupError('Eve auth.json must not be a hard link.')
    signed_in = api_ready(destination)
    key = env.get('OPENAI_API_KEY', '').strip()
    if not signed_in and not key:
        raise SetupError('Set OPENAI_API_KEY in the Eve env file before running codex-home.')
    # Prepare copies before replacing existing runtime directories. auth.json is
    # never a copy target. Refreshing an existing home preserves its API sign-in.
    with tempfile.TemporaryDirectory(prefix='.codex-setup-', dir=destination.parent) as tmp:
        staging = Path(tmp)
        for relative in COPY_PATHS:
            path = source / relative
            if path.exists():
                _copy_runtime(path, staging / relative, source.resolve(), destination.resolve())
        _api_config(staging / 'config.toml')
        for relative in COPY_PATHS:
            copied, target = staging / relative, destination / relative
            if not copied.exists():
                continue
            if any(p.is_symlink() for p in (target, *target.parents)):
                raise SetupError('Eve runtime destination must not contain symlinks.')
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.is_dir():
                shutil.rmtree(target)
            elif target.exists():
                target.unlink()
            shutil.move(str(copied), target)
    if signed_in:
        return destination
    child = {k: env[k] for k in ENV_KEYS if k in env}
    child['CODEX_HOME'] = str(destination)
    try:
        # No shell, no key in argv, and no raw login output in logs (even on error).
        result = subprocess.run([binary, 'login', '--with-api-key'], input=key + '\n',
                                text=True, env=child, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise SetupError('Codex API login could not run; check the installed Codex executable.') from exc
    if result.returncode or not api_ready(destination):
        raise SetupError('Codex API login failed; Eve will keep using the plan until setup succeeds.')
    (destination / 'auth.json').chmod(0o600)
    return destination
