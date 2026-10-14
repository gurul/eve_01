"""The env writer eve lite's setup page saves the OpenRouter key with: envfile.set_key and remove_key.

Atomic, owner-only (0600), every other line kept, and whatever it writes reads back unchanged through the loader the
daemon uses. Values never reach a log line.
"""

from __future__ import annotations

import logging
import os
import stat
import threading
from pathlib import Path

import pytest

from eve_bridge import envfile
from eve_bridge.envfile import env_path, load_env_file, parse_env_file, remove_key, set_key

CANARY = "CANARY-NOT-A-KEY-7f3c"


def mode(path: Path) -> int:
    return stat.S_IMODE(os.stat(path).st_mode)


def leftovers(folder: Path) -> list[str]:
    return sorted(p.name for p in folder.iterdir() if p.name.endswith(".tmp"))


def test_a_new_file_is_private_and_so_is_a_new_folder(tmp_path: Path) -> None:
    f = tmp_path / "cfg" / "eve-bridge" / "env"
    set_key(f, "OPENROUTER_API_KEY", CANARY)
    assert f.read_text() == f"OPENROUTER_API_KEY={CANARY}\n"
    assert mode(f) == 0o600
    assert mode(f.parent) == 0o700
    assert leftovers(f.parent) == []


def test_a_key_is_replaced_where_it_is_and_every_other_line_is_kept(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("# eve lite\n"
                 "EVE_PROFILE=lite\n"
                 "\n"
                 "export OPENROUTER_API_KEY=old-value\n"
                 "a line the parser skips\n"
                 "OTHER='x y'\n")
    os.chmod(f, 0o644)
    set_key(f, "OPENROUTER_API_KEY", CANARY)
    assert f.read_text() == ("# eve lite\n"
                             "EVE_PROFILE=lite\n"
                             "\n"
                             f"OPENROUTER_API_KEY={CANARY}\n"
                             "a line the parser skips\n"
                             "OTHER='x y'\n")
    assert mode(f) == 0o600                     # tightened, whatever it was
    assert leftovers(tmp_path) == []


def test_a_name_set_twice_ends_as_one_line_where_it_first_was(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("A=1\nK=first\nB=2\nK=second\n")
    assert parse_env_file(f.read_text())["K"] == "second"        # the loader reads the last one
    set_key(f, "K", "new")
    assert f.read_text() == "A=1\nK=new\nB=2\n"


def test_a_new_name_goes_at_the_end_even_without_a_final_newline(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("EVE_PROFILE=lite")
    set_key(f, "OPENROUTER_API_KEY", CANARY)
    assert f.read_text() == f"EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n"


def test_a_name_inside_another_name_is_left_alone(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("MY_OPENROUTER_API_KEY=keep\nOPENROUTER_API_KEY_OLD=keep\n# OPENROUTER_API_KEY=commented\n")
    set_key(f, "OPENROUTER_API_KEY", CANARY)
    assert f.read_text() == ("MY_OPENROUTER_API_KEY=keep\nOPENROUTER_API_KEY_OLD=keep\n"
                             f"# OPENROUTER_API_KEY=commented\nOPENROUTER_API_KEY={CANARY}\n")


@pytest.mark.parametrize("value", [
    CANARY, "with spaces inside", " leading", "trailing ", "\ttabbed\t", "'single'", '"double"', "it's",
    'say "hi"', "a=b#c", "", "'", '""', "''", "x ", "\x1f", "$HOME", "ünïcode ✓",
])
def test_every_value_reads_back_unchanged(tmp_path: Path, value: str) -> None:
    f = tmp_path / "env"
    f.write_text("BEFORE=1\n")
    set_key(f, "V", value)
    assert parse_env_file(f.read_text(encoding="utf-8"))["V"] == value
    env: dict[str, str] = {}
    load_env_file(f, env)
    assert env == {"BEFORE": "1", "V": value}


def test_a_plain_value_is_written_bare(tmp_path: Path) -> None:
    f = tmp_path / "env"
    set_key(f, "V", "plain value")
    set_key(f, "Q", " needs quotes")
    assert f.read_text() == 'V=plain value\nQ=" needs quotes"\n'


@pytest.mark.parametrize("bad", sorted(envfile._LINE_BREAKS) + ["\0"])
def test_a_value_that_would_start_another_line_is_refused(tmp_path: Path, bad: str) -> None:
    f = tmp_path / "env"
    f.write_text("KEEP=1\n")
    before = f.read_bytes()
    if bad != "\0":
        assert len(f"A=x{bad}EVIL=y".splitlines()) == 2      # control: the loader would read a second line
    with pytest.raises(ValueError) as e:
        set_key(f, "OPENROUTER_API_KEY", f"x{bad}EVIL=y")
    assert "EVIL" not in str(e.value)
    assert f.read_bytes() == before
    assert leftovers(tmp_path) == []


def test_the_refused_set_is_every_line_break_the_parser_knows() -> None:
    breaks = {chr(i) for i in range(0x110000) if len(f"a{chr(i)}b".splitlines()) > 1}      # all of Unicode
    assert breaks == set(envfile._LINE_BREAKS)


@pytest.mark.parametrize("name", ["", "1BAD", "A-B", "A B", "NAME\n", "export X", "Ä", None])
def test_a_name_the_parser_would_not_read_is_refused(tmp_path: Path, name) -> None:
    f = tmp_path / "env"
    with pytest.raises(ValueError):
        set_key(f, name, "v")
    with pytest.raises(ValueError):
        remove_key(f, name)
    assert not f.exists()


def test_a_symlinked_file_is_written_through_to_its_target(tmp_path: Path) -> None:
    real = tmp_path / "dotfiles" / "eve-env"
    real.parent.mkdir()
    real.write_text("A=1\n")
    link = tmp_path / "env"
    link.symlink_to(real)
    set_key(link, "OPENROUTER_API_KEY", CANARY)
    assert link.is_symlink()
    assert real.read_text() == f"A=1\nOPENROUTER_API_KEY={CANARY}\n"
    assert mode(real) == 0o600


def test_a_failed_write_keeps_the_old_file_and_leaves_no_temporary(tmp_path: Path,
                                                                    monkeypatch: pytest.MonkeyPatch) -> None:
    f = tmp_path / "env"
    f.write_text("EVE_PROFILE=lite\n")

    def fail(*_a, **_k):
        raise OSError("disk full")

    monkeypatch.setattr(envfile.os, "replace", fail)
    with pytest.raises(OSError):
        set_key(f, "OPENROUTER_API_KEY", CANARY)
    assert f.read_text() == "EVE_PROFILE=lite\n"
    assert leftovers(tmp_path) == []


def test_a_file_that_cannot_be_read_is_never_overwritten(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_bytes(b"A=1\n\xff\xfe broken\n")
    with pytest.raises(ValueError):                      # UnicodeDecodeError
        set_key(f, "OPENROUTER_API_KEY", CANARY)
    with pytest.raises(ValueError):
        remove_key(f, "A")
    assert f.read_bytes() == b"A=1\n\xff\xfe broken\n"


def test_remove_key_deletes_every_line_for_the_name(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("# keep me\nEVE_PROFILE=lite\nOPENROUTER_API_KEY=a\nexport EVE_PROFILE=full\nB=2\n")
    os.chmod(f, 0o644)
    assert remove_key(f, "EVE_PROFILE") is True
    assert f.read_text() == "# keep me\nOPENROUTER_API_KEY=a\nB=2\n"
    assert mode(f) == 0o600
    assert leftovers(tmp_path) == []


def test_remove_key_writes_nothing_when_the_name_is_not_there(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("A=1\n")
    os.chmod(f, 0o644)
    stamp = os.stat(f).st_mtime_ns
    assert remove_key(f, "OPENROUTER_API_KEY") is False
    assert f.read_text() == "A=1\n" and os.stat(f).st_mtime_ns == stamp
    assert mode(f) == 0o644                              # nothing was written, so nothing was changed


def test_remove_key_on_a_missing_file_creates_nothing(tmp_path: Path) -> None:
    f = tmp_path / "no-such" / "env"
    assert remove_key(f, "EVE_PROFILE") is False
    assert not f.parent.exists()


def test_the_daemon_loads_what_set_key_wrote(tmp_path: Path) -> None:
    f = tmp_path / "env"
    set_key(f, "EVE_PROFILE", "lite")
    set_key(f, "OPENROUTER_API_KEY", CANARY)
    env: dict[str, str] = {}
    assert load_env_file(f, env) == ["EVE_PROFILE", "OPENROUTER_API_KEY"]
    assert env == {"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": CANARY}


def test_env_path_is_the_file_load_env_file_reads(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    shared = tmp_path / "shared-env"
    environ = {"EVE_ENV_FILE": str(shared)}
    assert env_path(environ) == shared
    set_key(env_path(environ), "K", "from-the-writer")
    load_env_file(environ=environ)
    assert environ["K"] == "from-the-writer"
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    assert env_path({}) == tmp_path / "home" / ".config" / "eve-bridge" / "env"
    set_key(env_path({}), "K", "default")
    plain: dict[str, str] = {}
    load_env_file(environ=plain)
    assert plain == {"K": "default"}


def test_concurrent_writers_lose_no_line(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("EVE_PROFILE=lite\n")

    def write(n: int) -> None:
        for i in range(20):
            set_key(f, f"K_{n}_{i}", str(i))

    threads = [threading.Thread(target=write, args=(n,)) for n in range(6)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    parsed = parse_env_file(f.read_text())
    assert len(parsed) == 1 + 6 * 20 and parsed["EVE_PROFILE"] == "lite"
    assert leftovers(tmp_path) == []


def test_only_the_name_reaches_the_log(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.DEBUG)
    f = tmp_path / "env"
    logging.getLogger("eve_bridge.envfile").debug("control %s", CANARY)
    assert any(CANARY in r.getMessage() for r in caplog.records)          # the capture sees a planted canary
    caplog.clear()
    set_key(f, "OPENROUTER_API_KEY", CANARY)
    set_key(f, "OPENROUTER_API_KEY", CANARY + "-2")
    assert remove_key(f, "OPENROUTER_API_KEY")
    with pytest.raises(ValueError):
        set_key(f, "OPENROUTER_API_KEY", CANARY + "\n")
    messages = [r.getMessage() for r in caplog.records]
    assert messages and all("OPENROUTER_API_KEY" in m for m in messages)
    assert not any(CANARY in m for m in messages)
