"""Regenerate decode_frame.json: decode_frame's outcome on odd inputs, as CPython runs it.

Run with the daemon's interpreter (3.13):
    ../eve/bridge/.venv/bin/python rust/crates/eve-vision/tests/fixtures/gen_decode.py
"""
import base64
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))
from eve_bridge.vision import decode_frame  # noqa: E402

GRAY4 = base64.b64encode(bytes(4)).decode()
JPEG = base64.b64encode(b"\xff\xd8\xff\xd9").decode()


def g(**kw):
    o = {"seq": 1, "w": 2, "h": 2, "fmt": "gray", "b64": GRAY4}
    o.update(kw)
    return o


B64S = ["QUJD", "QUJ=", "QR==", "QUJD=", "QUJD==", "QUI==", "QQ=", "QQ", "QUJDQ=", "=", "==", "QQ==QQ==",
        "QQ==\n", "Q===", "QUJD\n", " QUJD", "QU JD", "QUJD====", "Q", "QU", "QUJ", "QUJDQ", "QUJDQU",
        "QUJDQUI", "QUJDQUJD", "AA=A", "A=AA", "AA==AA", "é", "QUJDé", "-_-_", "QQ=\n=", "QUJDQ==",
        "////", "+++/", "QUJD=QUJD", "\x00",
        "A=AA", "A=A", "A=", "A==", "A=AAA", "AA=AA", "AAA=A", "AA==A", "AAA==", "AAAA=", "AB=C=", "A=B=C",
        "A=\n", "A\n=", "AA=\nA", "AA==\n", "\nAA==", "AAA=\n", "A=A=", "A==A", "AA=", "AAA=", "AAAAA=",
        "AAAAA==", "AA*=", "AA=*", "AAAAA=AA", "AAAAAA=A", "AAAAAAA=", "AAAAAAAA=="]

cases = []
for s in B64S:
    cases.append({"seq": 1, "w": 1, "h": 1, "fmt": "jpeg", "b64": s})
cases += [
    g(seq="7"), g(seq=" 7 "), g(seq="+7"), g(seq="-7"), g(seq="1_000"), g(seq="1__0"), g(seq="_1"), g(seq="1_"),
    g(seq="7.0"), g(seq=""), g(seq="  "), g(seq=7.9), g(seq=-7.9), g(seq=True), g(seq=False), g(seq=None),
    g(seq=[1]), g(seq={"a": 1}), g(seq="0x10"), g(seq="\t8\n"), g(seq=1e300), g(w="2", h="2"),
    g(w=0), g(w=-1), g(h=0), g(w=2.5), g(w=3), g(w=1, h=4), g(fmt="rgb"), g(fmt=None), g(fmt=""),
    g(fmt=0), g(fmt=1), g(fmt=1.5), g(fmt=True), g(fmt=False), g(fmt=[]), g(fmt=["a"]), g(fmt={"k": "v"}),
    g(fmt="it's"), g(fmt='say "hi"'), g(fmt="a\nb"), g(fmt="GRAY"), g(b64=None), g(b64=""), g(b64=5),
    g(b64=["x"]), g(yaw=1, pitch=2.5), g(yaw="1", pitch=None), g(yaw=True, pitch=False), g(snap=True),
    g(snap=1), g(snap="true"), g(snap=False),
    {"seq": 1, "w": 1, "h": 1, "fmt": "jpeg", "b64": JPEG},
    {"seq": 1, "w": 1, "h": 1, "fmt": "jpeg", "b64": base64.b64encode(b"\xff").decode()},
    {"seq": 1, "w": 1, "h": 1, "fmt": "jpeg", "b64": base64.b64encode(b"PNG").decode()},
    {"w": 2, "h": 2}, {"seq": 1, "h": 2}, {"seq": 1, "w": 2}, {}, [], [1], "frame", 5, None, True, 2.5,
]

out = []
for c in cases:
    try:
        f = decode_frame(c)
        out.append({"in": c, "ok": {"seq": f.seq, "w": f.w, "h": f.h, "fmt": f.fmt,
                                     "data": base64.b64encode(f.data).decode(),
                                     "yaw": f.yaw, "pitch": f.pitch, "snap": f.snap}})
    except ValueError as e:
        out.append({"in": c, "err": str(e)})
    except Exception as e:  # noqa: BLE001
        out.append({"in": c, "raises": type(e).__name__, "msg": str(e)})

(HERE / "decode_frame.json").write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
print(len(out), "cases")
