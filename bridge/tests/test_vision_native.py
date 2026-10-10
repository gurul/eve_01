"""Exercise the actual PyObjC/Vision boundary on macOS, including its options bridge."""

import math
import sys

import pytest

from eve_bridge.identity import vision_feature_print
from eve_bridge.vision import Frame, Rect, vision_detect

pytestmark = pytest.mark.skipif(sys.platform != "darwin", reason="requires macOS Vision")


@pytest.fixture
def native_frame() -> Frame:
    pytest.importorskip("Vision")
    pytest.importorskip("Quartz")
    return Frame(1, 160, 120, "gray", bytes([128]) * (160 * 120))


def test_native_face_request_accepts_default_options(native_frame: Frame) -> None:
    # A blank image has no face, but must still construct and run the request.
    assert vision_detect(native_frame) == []


def test_native_identity_request_accepts_default_options(native_frame: Frame) -> None:
    vector = vision_feature_print(native_frame, Rect(20, 20, 80, 80))
    assert vector and all(math.isfinite(value) for value in vector)
