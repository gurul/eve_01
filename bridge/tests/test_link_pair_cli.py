"""``eve-bridge link pair``: Eve Link's pairing link without Telegram (app-only since 2026-10-08)."""

from __future__ import annotations

import io
import json

from eve_bridge import stick_link

TOKEN = "t" * 43


class _Reply(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def _ndjson(*messages: str) -> bytes:
    rows = [{"event": "open"}] + [{"event": "message", "message": m} for m in messages]
    return "\n".join(json.dumps(r) for r in rows).encode()


def test_the_newest_quick_tunnel_address_is_read_from_the_rendezvous() -> None:
    seen: list[str] = []

    def opener(url, timeout):
        seen.append(url)
        return _Reply(_ndjson("https://old-one.trycloudflare.com", "https://new-one.trycloudflare.com/"))

    got = stick_link.latest_address(TOKEN, "https://ntfy.example", opener=opener)
    assert got == "https://new-one.trycloudflare.com"
    assert seen == [f"https://ntfy.example/{stick_link.rendezvous_topic(TOKEN)}/json?poll=1&since=all"]
    assert TOKEN not in seen[0]                                   # the topic, never the token


def test_anything_but_a_bare_quick_tunnel_origin_is_ignored() -> None:
    def opener(url, timeout):
        return _Reply(_ndjson("https://new-one.trycloudflare.com", "https://evil.example.com",
                              "https://x.trycloudflare.com/path", "not a url"))

    assert stick_link.latest_address(TOKEN, "https://ntfy.example", opener=opener) == "https://new-one.trycloudflare.com"


def test_no_address_or_a_failed_read_is_empty() -> None:
    def empty(url, timeout):
        return _Reply(_ndjson())

    def broken(url, timeout):
        raise OSError("no route")

    assert stick_link.latest_address(TOKEN, "https://ntfy.example", opener=empty) == ""
    assert stick_link.latest_address(TOKEN, "https://ntfy.example", opener=broken) == ""
    assert stick_link.latest_address("", "https://ntfy.example", opener=empty) == ""


def test_link_pair_prints_the_pairing_link(monkeypatch, capsys, tmp_path) -> None:
    from eve_bridge import cli, notify

    monkeypatch.setattr(notify, "token_path", lambda: tmp_path / "stick-link.json")
    stick_link.LinkToken(tmp_path / "stick-link.json").ensure()
    monkeypatch.setattr(stick_link, "latest_address", lambda token, base, **k: "https://new-one.trycloudflare.com")
    monkeypatch.setattr("shutil.which", lambda name: None)
    assert cli.main(["link", "pair"]) == 0
    out = capsys.readouterr().out
    assert "https://new-one.trycloudflare.com" + stick_link.PAIR_PATH + "#t=" in out


def test_link_pair_says_so_when_no_address_is_posted(monkeypatch, capsys, tmp_path) -> None:
    from eve_bridge import cli, notify

    monkeypatch.setattr(notify, "token_path", lambda: tmp_path / "stick-link.json")
    monkeypatch.setattr(stick_link, "latest_address", lambda token, base, **k: "")
    assert cli.main(["link", "pair"]) == 1
    assert "isn't posted yet" in capsys.readouterr().out
