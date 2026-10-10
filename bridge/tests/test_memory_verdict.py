"""Each memory switch's default in code equals the Defaults table in the memory eval's VERDICT.md, and every
feature the table names has its holdout run beside it."""

from __future__ import annotations

import re
from pathlib import Path

from eve_bridge import memory_index, records

EVAL = Path(__file__).parent / "fixtures" / "memory_eval"
ROW = re.compile(r"^\|\s*`?([a-z_]+)`?\s*\|\s*`?(EVE_MEMORY_[A-Z_]+)`?\s*\|\s*(ON|OFF)\s*\|", re.M)


def verdict_defaults() -> dict[str, tuple[str, bool]]:
    text = (EVAL / "VERDICT.md").read_text(encoding="utf-8")
    section = text.split("## Defaults", 1)[1]
    return {name: (var, state == "ON") for name, var, state in ROW.findall(section)}


def code_defaults() -> dict[str, tuple[str, bool]]:
    out = {k: (v, memory_index.DEFAULTS[k]) for k, v in memory_index.SWITCHES.items()}
    out["validity"] = (records.VALIDITY_ENV, records.DEFAULTS[records.VALIDITY_ENV])
    out["sources"] = (records.SOURCES_ENV, records.DEFAULTS[records.SOURCES_ENV])
    return out


def test_every_switch_default_equals_the_verdict() -> None:
    assert verdict_defaults() == code_defaults()


def test_an_unset_switch_reads_its_default_and_a_set_one_reads_its_value() -> None:
    for name, (var, default) in code_defaults().items():
        read = memory_index.switch if name in memory_index.SWITCHES else records._switch
        key = name if name in memory_index.SWITCHES else var
        assert read(key, {}) is default
        assert read(key, {var: ""}) is default
        assert read(key, {var: "1"}) is True
        assert read(key, {var: "0"}) is False


def test_each_feature_in_the_verdict_has_its_holdout_run() -> None:
    for name in verdict_defaults():
        assert (EVAL / f"holdout_{name}.json").is_file(), name
    assert (EVAL / "holdout_baseline.json").is_file()
