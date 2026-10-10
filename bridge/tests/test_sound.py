"""sound.py: the persisted mute choice and the wire helpers."""

from __future__ import annotations

from eve_bridge.sound import SoundSetting, build_sound_cmd, default_path, quiet_caption


def test_missing_file_means_sound_on(tmp_path) -> None:
    s = SoundSetting(tmp_path / "sound.json")
    assert s.load() is True and not s.muted


def test_mute_persists_across_instances(tmp_path) -> None:
    p = tmp_path / "cfg" / "sound.json"
    s = SoundSetting(p)
    assert s.set(False) is True            # changed
    assert s.set(False) is False           # already muted
    again = SoundSetting(p)
    assert again.load() is False and again.muted
    again.set(True)
    assert SoundSetting(p).load() is True


def test_corrupt_file_keeps_sound_on(tmp_path) -> None:
    p = tmp_path / "sound.json"
    p.write_text("{not json")
    assert SoundSetting(p).load() is True


def test_non_object_json_does_not_crash_microphone_or_sound_startup(tmp_path, caplog) -> None:
    p = tmp_path / "switch.json"
    for name in ("mic", "sound"):
        for text in ("null", "[]", "false", "42", '"muted"'):
            p.write_text(text)
            assert SoundSetting(p, name=name).load() is True
    assert "expected a JSON object" in caplog.text
    # A valid stored setting is still honored.
    p.write_text('{"muted": true}')
    assert SoundSetting(p).load() is False


def test_wire_helpers() -> None:
    assert build_sound_cmd(False) == {"cmd": "sound", "on": False}
    page = {"cmd": "caption", "page": 0, "chirp": True}
    assert quiet_caption(page, muted=True) == {"cmd": "caption", "page": 0, "chirp": False}
    assert quiet_caption(page, muted=False) is page
    assert page["chirp"] is True           # the original is never modified


def test_default_path_env_override(tmp_path) -> None:
    assert default_path({"EVE_SOUND_FILE": str(tmp_path / "x.json")}) == tmp_path / "x.json"
    assert default_path({}).name == "sound.json"
