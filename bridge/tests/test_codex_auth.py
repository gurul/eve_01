"""Private identity setup with a real fake CLI; no owner credentials or network."""
import json
import os
import stat
import sys
from pathlib import Path

import pytest

from eve_bridge import codex_auth


@pytest.fixture
def homes(tmp_path, monkeypatch):
    monkeypatch.setenv('HOME', str(tmp_path))
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    monkeypatch.setattr(codex_auth, '_warned', set())
    source = tmp_path / '.codex'
    source.mkdir()
    personal = json.dumps({'auth_mode': 'chatgpt', 'tokens': {'access_token': 'personal-test-token'}})
    (source / 'auth.json').write_text(personal)
    (source / 'config.toml').write_text('model = "test-model"\n')
    return source, codex_auth.eve_home()


def ready(home):
    home.mkdir(parents=True, exist_ok=True)
    (home / 'auth.json').write_text(json.dumps({'auth_mode': 'apikey', 'OPENAI_API_KEY': 'fixture-api-value'}))


def fake_binary(tmp_path):
    binary = tmp_path / 'codex-fake'
    binary.write_text(f'''#!{sys.executable}
import json, os, sys
from pathlib import Path
h = Path(os.environ['CODEX_HOME'])
value = sys.stdin.read().strip()
(h / 'test-login-call.json').write_text(json.dumps({{'argv': sys.argv[1:], 'stdin': value,
    'env_names': list(os.environ), 'home': str(h)}}))
(h / 'auth.json').write_text(json.dumps({{'auth_mode': 'apikey', 'OPENAI_API_KEY': value}}))
''')
    binary.chmod(0o700)
    return str(binary)


def test_default_plan_environment_is_exactly_unchanged(homes):
    env = dict(HOME='test-home', PATH='test-path', USER='user', LOGNAME='login', TMPDIR='temp', LANG='C',
               CODEX_HOME='personal-home', OPENAI_API_KEY='test-secret', EVE_TELEGRAM_TOKEN='bot',
               CODEX_APP_TOOLS_PIPE_PATH='desktop-pipe')
    expected = {k: env[k] for k in codex_auth.ENV_KEYS}
    assert codex_auth.launch_environment(env) == (expected, 'plan')
    assert codex_auth.launch_environment({**env, 'EVE_CODEX_AUTH': 'plan'}) == (expected, 'plan')


def test_api_ready_selects_only_private_home(homes):
    _, private = homes
    ready(private)
    env, mode = codex_auth.launch_environment({'EVE_CODEX_AUTH': 'api', 'HOME': 'os-home',
                                              'CODEX_HOME': 'personal', 'OPENAI_API_KEY': 'test-secret'})
    assert mode == 'api'
    assert env == {'HOME': 'os-home', 'CODEX_HOME': str(private)}


@pytest.mark.parametrize('auth', [None, 'broken-json', '{}', '[]',
    '{"auth_mode":"chatgpt","OPENAI_API_KEY":"not-api"}',
    '{"auth_mode":"apikey","OPENAI_API_KEY":""}',
    '{"auth_mode":"apikey","OPENAI_API_KEY":17}'])
def test_missing_or_unsigned_home_falls_back_and_logs_once(homes, caplog, auth):
    _, private = homes
    if auth is not None:
        private.mkdir(parents=True)
        (private / 'auth.json').write_text(auth)
    for _ in range(3):
        assert codex_auth.launch_environment({'EVE_CODEX_AUTH': 'api', 'HOME': 'original'}) == (
            {'HOME': 'original'}, 'plan')
    assert len(caplog.records) == 1
    assert 'using the ChatGPT plan' in caplog.text and 'codex-home' in caplog.text


@pytest.mark.parametrize('mode', ['', 'API', 'automatic'])
def test_invalid_setting_falls_back(homes, caplog, mode):
    assert codex_auth.launch_environment({'EVE_CODEX_AUTH': mode}) == ({}, 'plan')
    assert 'must be plan or api' in caplog.text


def test_setup_copies_runtime_not_auth_and_passes_key_only_on_stdin(homes, tmp_path):
    source, private = homes
    personal = (source / 'auth.json').read_bytes()
    original_config = ('model = "test-model"\ncli_auth_credentials_store = "keyring"\n'
                       'forced_login_method = "chatgpt"\nforced_chatgpt_workspace_id = "workspace"\n'
                       '[mcp_servers.cua_repl.env]\nCODEX_HOME = ' + json.dumps(str(source)) + '\n')
    (source / 'config.toml').write_text(original_config)
    for relative in ('computer-use/config.json', 'browser/sessions/example.toml',
                     'plugins/cache/sample/.mcp.json', '.tmp/bundled-marketplaces/catalog.json',
                     'chrome-native-hosts-v2.json'):
        p = source / relative
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text('{}' if p.suffix == '.json' else 'active = true\n')
    (source / 'plugins/auth.json').write_text('must-not-copy')
    (source / 'plugins/.credentials.json').write_text('must-not-copy')
    (source / 'history.jsonl').write_text('private-history')
    result = codex_auth.setup(binary=fake_binary(tmp_path), environ={'OPENAI_API_KEY': 'fixture-secret'})
    assert result == private and codex_auth.api_ready(private)
    assert stat.S_IMODE(private.stat().st_mode) == 0o700
    assert stat.S_IMODE((private / 'auth.json').stat().st_mode) == 0o600
    assert (source / 'auth.json').read_bytes() == personal
    assert (source / 'config.toml').read_text() == original_config
    assert not (private / 'plugins/auth.json').exists()
    assert not (private / 'plugins/.credentials.json').exists()
    assert not (private / 'history.jsonl').exists()
    for relative in codex_auth.COPY_PATHS:
        assert (private / relative).exists()
    call = json.loads((private / 'test-login-call.json').read_text())
    assert call['argv'] == ['login', '--with-api-key']
    assert call['stdin'] == 'fixture-secret' and call['home'] == str(private)
    assert 'OPENAI_API_KEY' not in call['env_names']
    config = (private / 'config.toml').read_text()
    assert 'cli_auth_credentials_store = "file"' in config and 'forced_login_method = "api"' in config
    assert 'forced_chatgpt_workspace_id' not in config
    assert str(source) not in config and str(private) in config
    assert 'fixture-secret' not in config
    assert (private / 'auth.json').read_bytes() != personal  # positive control: distinct identities


def test_repeat_refreshes_config_and_preserves_api_signin_without_key(homes, tmp_path):
    source, private = homes
    codex_auth.setup(binary=fake_binary(tmp_path), environ={'OPENAI_API_KEY': 'fixture-secret'})
    auth = (private / 'auth.json').read_bytes()
    (private / 'test-login-call.json').unlink()
    (source / 'config.toml').write_text('model = "new-test-model"\n')
    codex_auth.setup(binary='must-not-be-executed', environ={})
    assert (private / 'auth.json').read_bytes() == auth
    assert not (private / 'test-login-call.json').exists()
    assert 'new-test-model' in (private / 'config.toml').read_text()


def test_no_key_means_no_login(homes):
    with pytest.raises(codex_auth.SetupError, match='Set OPENAI_API_KEY'):
        codex_auth.setup(binary='must-not-run', environ={})


def test_failed_login_does_not_echo_output_or_key(homes, tmp_path, capsys):
    bad = tmp_path / 'codex-bad'
    bad.write_text(f'#!{sys.executable}\nimport sys\nprint(sys.stdin.read())\nsys.exit(1)\n')
    bad.chmod(0o700)
    with pytest.raises(codex_auth.SetupError, match='login failed') as err:
        codex_auth.setup(binary=str(bad), environ={'OPENAI_API_KEY': 'fixture-secret'})
    assert 'fixture-secret' not in str(err.value)
    assert 'fixture-secret' not in str(capsys.readouterr())
    assert not codex_auth.api_ready(homes[1])


def test_destination_symlink_cannot_reach_personal_home(homes, tmp_path):
    source, private = homes
    before = (source / 'auth.json').read_bytes()
    private.parent.mkdir(parents=True)
    private.symlink_to(source, target_is_directory=True)
    with pytest.raises(codex_auth.SetupError):
        codex_auth.setup(binary='must-not-run', environ={'OPENAI_API_KEY': 'fixture-secret'})
    assert (source / 'auth.json').read_bytes() == before
    assert not codex_auth.api_ready(private)


def test_source_runtime_link_to_credentials_is_refused(homes):
    source, _ = homes
    (source / 'plugins').mkdir()
    (source / 'plugins/innocent.json').symlink_to(source / 'auth.json')
    with pytest.raises(codex_auth.SetupError, match='credentials'):
        codex_auth.setup(binary='must-not-run', environ={'OPENAI_API_KEY': 'fixture-secret'})


def test_hard_linked_auth_is_never_written(homes):
    source, private = homes
    private.mkdir(parents=True)
    before = (source / 'auth.json').read_bytes()
    os.link(source / 'auth.json', private / 'auth.json')
    with pytest.raises(codex_auth.SetupError, match='hard link'):
        codex_auth.setup(binary='must-not-run', environ={'OPENAI_API_KEY': 'fixture-secret'})
    assert not codex_auth.api_ready(private)
    assert (source / 'auth.json').read_bytes() == before


def test_source_hard_link_to_auth_is_not_copied(homes):
    source, _ = homes
    (source / 'plugins').mkdir()
    os.link(source / 'auth.json', source / 'plugins/alias.json')
    with pytest.raises(codex_auth.SetupError, match='hard link'):
        codex_auth.setup(binary='must-not-run', environ={'OPENAI_API_KEY': 'fixture-secret'})


def test_cyclic_runtime_link_fails_cleanly_before_login(homes):
    source, _ = homes
    (source / 'plugins').mkdir()
    (source / 'plugins/loop').symlink_to(source / 'plugins', target_is_directory=True)
    with pytest.raises(codex_auth.SetupError, match='cyclic'):
        codex_auth.setup(binary='must-not-run', environ={'OPENAI_API_KEY': 'fixture-secret'})


def test_internal_runtime_links_are_copied_not_shared(homes, tmp_path):
    source, private = homes
    version = source / 'plugins/cache/example/1.0'
    version.mkdir(parents=True)
    (version / '.mcp.json').write_text(json.dumps({'CODEX_HOME': str(source)}))
    (version.parent / 'latest').symlink_to(version, target_is_directory=True)
    codex_auth.setup(binary=fake_binary(tmp_path), environ={'OPENAI_API_KEY': 'fixture-secret'})
    copied = private / 'plugins/cache/example/latest'
    assert copied.is_dir() and not copied.is_symlink()
    assert json.loads((copied / '.mcp.json').read_text())['CODEX_HOME'] == str(private)
    assert json.loads((version / '.mcp.json').read_text())['CODEX_HOME'] == str(source)


def test_cli_setup_uses_loaded_env_and_does_not_change_mode(homes, tmp_path, monkeypatch, capsys):
    from eve_bridge import cli
    monkeypatch.setenv('EVE_CODEX_BIN', fake_binary(tmp_path))
    monkeypatch.setenv('OPENAI_API_KEY', 'fixture-secret')
    monkeypatch.delenv('EVE_CODEX_AUTH', raising=False)
    monkeypatch.setattr(cli, 'load_env_file', lambda: [])
    assert cli.main(['codex-home']) == 0
    assert 'API home ready' in capsys.readouterr().out
    assert 'EVE_CODEX_AUTH' not in os.environ
