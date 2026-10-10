"""Env file loader — parsing, precedence, and the missing-file case."""

from __future__ import annotations

from pathlib import Path

from eve_bridge.envfile import load_env_file, parse_env_file


def test_parse_basic_lines() -> None:
    text = (
        "# a comment\n"
        "\n"
        "OPENAI_API_KEY=sk-test-123\n"
        "export EVE_NOTES_MODEL=gpt-5-mini\n"
        "QUOTED=\"with spaces\"\n"
        "SINGLE='x=y'\n"
        "  SPACED =  value  \n"
    )
    assert parse_env_file(text) == {
        "OPENAI_API_KEY": "sk-test-123",
        "EVE_NOTES_MODEL": "gpt-5-mini",
        "QUOTED": "with spaces",
        "SINGLE": "x=y",
        "SPACED": "value",
    }


def test_parse_skips_malformed_lines() -> None:
    assert parse_env_file("no equals\n=novalue\n1BAD=x\nGOOD=1\n") == {"GOOD": "1"}


def test_load_fills_gaps_but_never_overrides(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("A=from-file\nB=from-file\n")
    env = {"A": "from-shell"}
    assert load_env_file(f, env) == ["B"]
    assert env == {"A": "from-shell", "B": "from-file"}


def test_load_missing_file_is_noop(tmp_path: Path) -> None:
    env: dict[str, str] = {}
    assert load_env_file(tmp_path / "absent", env) == []
    assert env == {}


def test_load_expands_user(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    (tmp_path / "env").write_text("K=v\n")
    env: dict[str, str] = {}
    assert load_env_file(Path("~/env"), env) == ["K"]
    assert env["K"] == "v"


def test_load_explicit_shared_env_override(tmp_path):
    shared = tmp_path / "windows-env"
    shared.write_text("EVE_LEARNING_PROVIDER=openrouter\nOPENROUTER_API_KEY=test-only\n", encoding="utf-8-sig")
    env = {"EVE_ENV_FILE": str(shared)}
    load_env_file(environ=env)
    assert env["EVE_LEARNING_PROVIDER"] == "openrouter"
    assert env["OPENROUTER_API_KEY"] == "test-only"


def test_explicit_path_wins_over_env_override(tmp_path):
    shared = tmp_path / "shared"
    explicit = tmp_path / "explicit"
    shared.write_text("A=shared\n")
    explicit.write_text("A=explicit\n")
    env = {"EVE_ENV_FILE": str(shared)}
    load_env_file(explicit, env)
    assert env["A"] == "explicit"
