"""Owner identity — the print store, the enrolment policy, the daemon edge.

No Vision framework here: prints are short fake vectors, distance is plain
Euclidean (what Vision computes anyway), and the daemon tests drive Daemon
methods against a stub transport the way test_vision does.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import os
import stat
from pathlib import Path
from types import SimpleNamespace

import pytest

from eve_bridge import identity
from eve_bridge.identity import (
    CAPACITY,
    EnrolmentGate,
    FaceIdentity,
    OwnerIdentity,
    configured_threshold,
    crop_rect,
    euclidean,
)
from eve_bridge.scene import SceneWatcher
from eve_bridge.vision import FaceTracker, Frame, Rect

W, H = 160, 120
OWNER = [1.0, 0.0, 0.0]
OWNER_NEAR = [0.9, 0.1, 0.0]      # d ≈ 0.14 from OWNER
STRANGER = [0.0, 1.0, 0.0]        # d ≈ 1.41 from OWNER


def _core(**kw) -> OwnerIdentity:
    kw.setdefault("threshold", 0.5)
    return OwnerIdentity(distance=euclidean, **kw)


def _gray_obj(seq: int = 1, **extra) -> dict:
    obj = {"seq": seq, "w": W, "h": H, "fmt": "gray",
           "b64": base64.b64encode(bytes(W * H)).decode()}
    obj.update(extra)
    return obj


def _frame(seq: int = 1) -> Frame:
    return Frame(seq=seq, w=W, h=H, fmt="gray", data=bytes(W * H))


BIG = Rect(40, 20, 48, 48)        # 30 % of frame width
SMALL = Rect(70, 50, 16, 16)      # 10 %


# ---- core: classify / enrol / replace --------------------------------------

def test_empty_store_is_unknown() -> None:
    assert _core().classify(OWNER) == ("unknown", None)


def test_classify_by_min_distance_against_threshold() -> None:
    c = _core()
    c.enrol(OWNER)
    who, d = c.classify(OWNER_NEAR)
    assert who == "owner" and d == pytest.approx(euclidean(OWNER, OWNER_NEAR))
    who, d = c.classify(STRANGER)
    assert who == "unknown" and d == pytest.approx(euclidean(OWNER, STRANGER))


def test_threshold_boundary_is_exclusive() -> None:
    c = _core(threshold=1.0)
    c.enrol([0.0, 0.0])
    assert c.classify([1.0, 0.0])[0] == "unknown"   # d == threshold
    assert c.classify([0.99, 0.0])[0] == "owner"


def test_enrol_returns_slot_and_copies_vector() -> None:
    c = _core()
    vec = [1.0, 2.0]
    assert c.enrol(vec) == 1
    vec[0] = 99.0
    assert c.prints == [[1.0, 2.0]]
    assert c.enrol([3.0, 4.0]) == 2
    assert len(c) == 2


def test_full_store_replaces_the_print_nearest_to_the_new_one() -> None:
    c = _core(capacity=3)
    c.enrol([0.0, 0.0])
    c.enrol([10.0, 0.0])
    c.enrol([20.0, 0.0])
    slot = c.enrol([10.5, 0.0])        # nearest is slot 2
    assert slot == 2
    assert c.prints == [[0.0, 0.0], [10.5, 0.0], [20.0, 0.0]]
    assert len(c) == 3


def test_default_capacity_is_24() -> None:
    assert CAPACITY == 24
    c = _core()
    for i in range(30):
        c.enrol([float(i)])
    assert len(c) == 24


def test_reset_clears_memory_and_file(tmp_path: Path) -> None:
    path = tmp_path / "owner.json"
    c = _core(path=path)
    c.enrol(OWNER)
    c.save()
    assert path.exists()
    assert c.reset() == 1
    assert len(c) == 0 and c.enrolled_at is None
    assert not path.exists()
    assert c.reset() == 0               # idempotent, no file


# ---- persistence ------------------------------------------------------------

def test_save_load_round_trip_mode_600(tmp_path: Path) -> None:
    path = tmp_path / "cfg" / "owner_faceprints.json"
    c = _core(path=path)
    c.enrol(OWNER, now=1000.0)
    c.enrol(STRANGER, now=1001.0)
    c.save()
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    raw = json.loads(path.read_text())
    assert raw["version"] == 1 and raw["revision"] == 2
    assert raw["prints"] == [OWNER, STRANGER]
    assert raw["enrolled_at"] == 1001.0

    d = _core(path=path)
    assert d.load() == 2
    assert d.prints == [OWNER, STRANGER]
    assert d.enrolled_at == 1001.0
    assert d.classify(OWNER_NEAR)[0] == "owner"


def test_load_missing_file_is_empty(tmp_path: Path) -> None:
    c = _core(path=tmp_path / "nope.json")
    assert c.load() == 0
    assert c.prints == []


@pytest.mark.parametrize("raw", [
    "not json",
    "[]",
    json.dumps({"version": 2, "revision": 2, "prints": []}),
    json.dumps({"version": 1, "revision": 1, "prints": [[1.0]]}),
    json.dumps({"version": 1, "revision": 2, "prints": "x"}),
    json.dumps({"version": 1, "revision": 2, "prints": [[1.0], [1.0, 2.0]]}),
    json.dumps({"version": 1, "revision": 2, "prints": [["a"]]}),
    json.dumps({"version": 1, "revision": 2, "prints": [[]]}),
])
def test_load_rejects_malformed_and_keeps_going(tmp_path: Path, raw: str, caplog) -> None:
    path = tmp_path / "owner.json"
    path.write_text(raw)
    c = _core(path=path)
    c.enrol(OWNER)
    with caplog.at_level(logging.WARNING, logger="eve_bridge.identity"):
        assert c.load() == 0
    assert "identity: ignoring" in caplog.text
    assert c.prints == [OWNER]           # a bad file does not wipe memory


def test_default_path_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_OWNER_PRINTS", raising=False)
    monkeypatch.setenv("XDG_CONFIG_HOME", "/x/cfg")
    assert identity.default_path() == Path("/x/cfg/eve-bridge/owner_faceprints.json")
    monkeypatch.delenv("XDG_CONFIG_HOME")
    assert identity.default_path() == Path.home() / ".config" / "eve-bridge" / "owner_faceprints.json"
    monkeypatch.setenv("EVE_OWNER_PRINTS", "~/mine.json")
    assert identity.default_path() == Path.home() / "mine.json"


def test_configured_threshold(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_OWNER_THRESHOLD", raising=False)
    assert configured_threshold() == identity.DEFAULT_THRESHOLD == 0.9
    monkeypatch.setenv("EVE_OWNER_THRESHOLD", "0.75")
    assert configured_threshold() == 0.75
    for bad in ("abc", "-1", "0", "nan"):
        monkeypatch.setenv("EVE_OWNER_THRESHOLD", bad)
        assert configured_threshold() == identity.DEFAULT_THRESHOLD


# ---- enrolment policy -------------------------------------------------------

def test_gate_requires_key_one_face_and_size() -> None:
    g = EnrolmentGate()
    assert g.allows(True, 1, 20, 0.0)
    assert not g.allows(False, 1, 30, 0.0)      # key up
    assert not g.allows(True, 2, 30, 0.0)       # two faces
    assert not g.allows(True, 0, 30, 0.0)       # no face
    assert not g.allows(True, 1, 19.9, 0.0)     # too small


def test_gate_one_enrolment_per_interval() -> None:
    g = EnrolmentGate()
    assert g.allows(True, 1, 30, 10.0)
    g.mark(10.0)
    assert not g.allows(True, 1, 30, 11.9)
    assert g.allows(True, 1, 30, 12.0)


# ---- FaceIdentity driver ----------------------------------------------------

def _driver(core: OwnerIdentity, vec, listen_down: bool = False, now: list[float] | None = None):
    now = now if now is not None else [100.0]
    state = {"down": listen_down}
    fi = FaceIdentity(core, describe=lambda f, r: vec, listen_down=lambda: state["down"],
                      clock=lambda: now[0])
    return fi, state, now


def test_process_enrols_only_while_key_down_with_one_big_face(tmp_path: Path, caplog) -> None:
    core = _core(path=tmp_path / "owner.json")
    fi, state, now = _driver(core, OWNER)
    with caplog.at_level(logging.INFO, logger="eve_bridge.identity"):
        assert fi.process(_frame(), [BIG]) == "unknown"          # key up: no enrol
        assert len(core) == 0
        state["down"] = True
        assert fi.process(_frame(), [BIG, SMALL]) == "unknown"   # two faces
        assert fi.process(_frame(), [SMALL]) == "unknown"        # too small
        assert len(core) == 0
        assert fi.process(_frame(), [BIG]) == "owner"            # enrolled, then matched
    assert len(core) == 1 and fi.enrolments == 1
    assert "identity: enrolled owner print #1 (size=30%, 1/24 held)" in caplog.text
    assert (tmp_path / "owner.json").exists()                    # persisted on enrolment


def test_process_rate_limits_enrolment_to_one_per_2s(tmp_path: Path) -> None:
    core = _core(path=tmp_path / "owner.json")
    fi, _, now = _driver(core, OWNER, listen_down=True)
    fi.process(_frame(), [BIG])
    now[0] += 1.0
    fi.process(_frame(), [BIG])
    assert len(core) == 1
    now[0] += 1.0
    fi.process(_frame(), [BIG])
    assert len(core) == 2


def test_process_no_face_or_no_print_is_unknown() -> None:
    core = _core()
    core.enrol(OWNER)
    fi, _, _ = _driver(core, None)
    assert fi.process(_frame(), []) == "unknown"
    assert fi.process(_frame(), [BIG]) == "unknown"
    assert fi.last_who is None


def test_process_describer_failure_is_logged_not_raised(caplog) -> None:
    def boom(f, r):
        raise RuntimeError("vision down")

    fi = FaceIdentity(_core(), describe=boom)
    with caplog.at_level(logging.ERROR, logger="eve_bridge.identity"):
        assert fi.process(_frame(), [BIG]) == "unknown"
    assert "feature print failed" in caplog.text


def test_transition_logs_are_rate_limited(caplog) -> None:
    core = _core()
    core.enrol(OWNER)
    seen = {"vec": STRANGER}
    now = [0.0]
    fi = FaceIdentity(core, describe=lambda f, r: seen["vec"], clock=lambda: now[0])
    with caplog.at_level(logging.INFO, logger="eve_bridge.identity"):
        fi.process(_frame(), [BIG])                 # unknown: first classification logs
        fi.process(_frame(), [BIG])                 # same: silent
        seen["vec"] = OWNER_NEAR
        now[0] = 10.0
        fi.process(_frame(), [BIG])                 # owner, inside 30 s: suppressed
        now[0] = 31.0
        seen["vec"] = STRANGER
        fi.process(_frame(), [BIG])                 # unknown again, window passed: logs
        now[0] = 62.0
        seen["vec"] = OWNER_NEAR
        fi.process(_frame(), [BIG])                 # owner: logs
    lines = [r.getMessage() for r in caplog.records if r.name == "eve_bridge.identity"]
    assert lines == [
        "identity: unknown face (d=1.41)",
        "identity: unknown face (d=1.41)",
        "identity: owner recognised (d=0.14)",
    ]
    assert fi.last_who == "owner"


def test_status_shape(tmp_path: Path) -> None:
    core = _core(path=tmp_path / "o.json", threshold=0.7)
    fi, _, _ = _driver(core, OWNER, listen_down=True)
    fi.process(_frame(), [BIG])
    st = fi.status()
    assert st["prints"] == 1 and st["capacity"] == 24 and st["threshold"] == 0.7
    assert st["path"] == str(tmp_path / "o.json")
    assert st["last_who"] == "owner" and st["last_distance"] == 0.0
    assert st["enrolments"] == 1 and isinstance(st["enrolled_at"], float)


# ---- crop geometry ----------------------------------------------------------

def test_crop_rect_pads_25_percent_and_clamps() -> None:
    assert crop_rect(Rect(40, 40, 40, 40), 160, 120) == (30, 30, 60, 60)
    assert crop_rect(Rect(0, 0, 40, 40), 160, 120) == (0, 0, 50, 50)          # top-left clamp
    assert crop_rect(Rect(130, 90, 40, 40), 160, 120) == (120, 80, 40, 40)   # bottom-right clamp


# ---- tracker + daemon wire edge --------------------------------------------

class _Sent:
    def __init__(self) -> None:
        self.items: list[dict] = []

    async def __call__(self, obj: dict) -> bool:
        self.items.append(obj)
        return True


class _FakeIdentity:
    def __init__(self, who: str) -> None:
        self.who = who
        self.calls: list[tuple[int, int]] = []

    def process(self, frame: Frame, rects: list[Rect]) -> str:
        self.calls.append((frame.seq, len(rects)))
        return self.who


def test_tracker_puts_identity_answer_in_who() -> None:
    fake = _FakeIdentity("owner")

    def detect(frame: Frame) -> list[Rect]:
        return [Rect(60, 40, 40, 40, conf=1.0)] if frame.seq == 1 else []

    async def go() -> list[dict]:
        sent = _Sent()
        t = FaceTracker(detect=detect, send=sent, identity=fake)
        await t.on_frame(_gray_obj(seq=1))
        await asyncio.sleep(0.05)
        await t.on_frame(_gray_obj(seq=2))
        await asyncio.sleep(0.05)
        t.stop()
        return sent.items

    sent = asyncio.run(go())
    assert sent == [
        {"cmd": "face", "seq": 1, "bx": 0, "by": 0, "size": 25, "conf": 100, "who": "owner"},
        {"cmd": "face", "seq": 2, "conf": 0, "who": "unknown"},
    ]
    assert fake.calls == [(1, 1)]        # identity never runs without a face


class _StubBle:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True


def _daemon(identity_obj, connected: bool = True) -> SimpleNamespace:
    from types import MethodType

    from eve_bridge.daemon import Daemon

    ble = _StubBle(connected)
    # _note_activity / _explorer: the listen key and every frame are also seen
    # by the idle explorer (explore.py); the stub keeps it inactive.
    # _head/_scene: every frame also carries the head pose (head.py) and is offered to
    # the voice's eyes (scene.py, inert outside a conversation).
    d = SimpleNamespace(ble=ble, _listen_sent=None, _listen_down=False, _identity=identity_obj,
                        _note_activity=lambda: None, _explorer=SimpleNamespace(active=False),
                        _head=SimpleNamespace(observe=lambda yaw, pitch: None), _scene=SceneWatcher(None))
    d._vision = FaceTracker(detect=lambda f: [Rect(60, 40, 40, 40, conf=1.0)], send=ble.send,
                            identity=identity_obj)
    for name in ("_on_listen_key", "_handle_identity", "_handle_ble"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def test_daemon_face_reply_carries_who_owner() -> None:
    async def go() -> list[dict]:
        d = _daemon(_FakeIdentity("owner"))
        await d._handle_ble({"frame": _gray_obj(seq=5, yaw=0, pitch=0)})
        await asyncio.sleep(0.05)
        d._vision.stop()
        return d.ble.sent

    assert asyncio.run(go()) == [
        {"cmd": "face", "seq": 5, "bx": 0, "by": 0, "size": 25, "conf": 100,
         "yaw": 0.0, "pitch": 0.0, "who": "owner"},
    ]


def test_daemon_tracks_listen_key_even_while_disconnected() -> None:
    d = _daemon(None, connected=False)
    d._on_listen_key(True)
    assert d._listen_down is True and d.ble.sent == []
    d._on_listen_key(False)
    assert d._listen_down is False


def test_daemon_identity_ipc_status_and_reset(tmp_path: Path) -> None:
    core = _core(path=tmp_path / "owner.json")
    fi = FaceIdentity(core, describe=lambda f, r: OWNER)
    core.enrol(OWNER)
    core.save()
    d = _daemon(fi)
    st = d._handle_identity("status")
    assert st["ok"] and st["enabled"] and st["identity"]["prints"] == 1
    rs = d._handle_identity("reset")
    assert rs == {"ok": True, "removed": 1}
    assert len(core) == 0 and not (tmp_path / "owner.json").exists()
    assert d._handle_identity("status")["identity"]["prints"] == 0


def test_daemon_identity_ipc_without_identity_reads_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = tmp_path / "owner.json"
    monkeypatch.setenv("EVE_OWNER_PRINTS", str(path))
    c = _core(path=path)
    c.enrol(OWNER)
    c.save()
    d = _daemon(None)
    st = d._handle_identity("status")
    assert st["enabled"] is False and st["identity"]["prints"] == 1
    assert d._handle_identity("reset") == {"ok": True, "removed": 1}
    assert not path.exists()


def test_make_describer_none_off_darwin(monkeypatch: pytest.MonkeyPatch, caplog) -> None:
    monkeypatch.setattr(identity.sys, "platform", "linux")
    with caplog.at_level(logging.WARNING, logger="eve_bridge.identity"):
        assert identity.make_describer() is None
    assert "needs macOS Vision" in caplog.text
