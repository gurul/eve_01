"""eve's local voice (kokoro_voice.py): English only, cut into pieces that stream, and OpenRouter's voice when
Kokoro is missing or fails before its first sound. No model is loaded: a fake engine stands in."""

from __future__ import annotations

import pytest

from eve_bridge import kokoro_voice, phone_call


class FakeEngine:
    def __init__(self, fail: bool = False) -> None:
        self.asked: list[tuple[str, str, str]] = []
        self.fail = fail

    def create(self, text, voice, speed, lang):
        if self.fail:
            raise RuntimeError("boom")
        self.asked.append((text, voice, lang))
        return [0.5, -2.0, 0.0], kokoro_voice.SAMPLE_RATE


def test_only_english_voices_and_an_english_phonemizer():
    assert kokoro_voice.english_voice("bm_george") == "bm_george"
    assert kokoro_voice.english_voice("AF_Bella") == "af_bella"
    for other in ("jf_alpha", "zf_xiaobei", "ef_dora", "", "af_", "af_../x"):
        assert kokoro_voice.english_voice(other) == kokoro_voice.DEFAULT_VOICE
    eng = FakeEngine()
    pcm = b"".join(kokoro_voice.KokoroVoice(eng, "hf_alpha").speak_stream("Hola, que tal."))
    assert eng.asked == [("Hola, que tal.", "af_heart", "en-us")]
    assert pcm == b"\xff\x3f\x01\x80\x00\x00"           # 0.5 -> 16383, -2.0 clipped to -32767, 0 -> 0


def test_a_reply_is_cut_into_pieces_that_keep_every_word():
    text = "Sure, done. The lights are off now, and I set an alarm for seven tomorrow morning! Anything else?"
    pieces = kokoro_voice.chunks(text)
    assert len(pieces) > 1 and " ".join(pieces) == text
    assert all(len(p) >= kokoro_voice.MIN_PIECE_CHARS for p in pieces)
    assert kokoro_voice.chunks("Hi.") == ["Hi."]
    assert kokoro_voice.chunks("  ") == []


def test_missing_model_files_mean_no_local_voice(tmp_path):
    assert kokoro_voice.load({"EVE_KOKORO_DIR": str(tmp_path)}) is None


def _voice(engine):
    v = phone_call.make_voice({"OPENROUTER_API_KEY": "or-key"})
    assert v.kokoro_env == {}                     # on by default
    v._kokoro, v._kokoro_tried = (kokoro_voice.KokoroVoice(engine) if engine else None), True
    v._openrouter_stream = lambda text: iter([b"OR"])
    return v


def test_kokoro_speaks_first_and_openrouter_takes_over_only_before_a_sound():
    assert b"".join(_voice(FakeEngine()).speak_stream("Hello there.")) != b"OR"
    assert b"".join(_voice(FakeEngine(fail=True)).speak_stream("Hello there.")) == b"OR"
    assert b"".join(_voice(None).speak_stream("Hello there.")) == b"OR"

    class Midway(FakeEngine):
        def create(self, text, voice, speed, lang):
            if self.asked:
                raise RuntimeError("second piece")
            return super().create(text, voice, speed, lang)

    with pytest.raises(RuntimeError, match="second piece"):
        b"".join(_voice(Midway()).speak_stream("This is the first piece, and this is the second piece."))
