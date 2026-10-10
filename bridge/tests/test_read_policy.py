"""is_within — the lexical containment check behind in-cwd Read deferral.

The dangerous failure modes here are prefix traps and relative paths: a
sibling that shares a prefix, or a path that is not absolute, is never inside.
"""

from __future__ import annotations

from eve_bridge.read_policy import is_within

# ---- is_within ----

def test_within_direct_child() -> None:
    assert is_within("/a/b/c.txt", "/a/b")


def test_within_equal_path() -> None:
    assert is_within("/a/b", "/a/b")


def test_not_within_sibling() -> None:
    assert not is_within("/a/bc/file", "/a/b")  # prefix trap: /a/bc is not in /a/b


def test_not_within_parent() -> None:
    assert not is_within("/a", "/a/b")


def test_relative_paths_rejected() -> None:
    assert not is_within("b/c.txt", "/a")
    assert not is_within("/a/b", "b")


def test_empty_rejected() -> None:
    assert not is_within("", "/a")
    assert not is_within("/a", "")
