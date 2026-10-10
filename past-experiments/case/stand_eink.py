"""Prop-up stand for the CrowPanel 4.2" e-ink pet — FreeCAD headless.

Run:  /Applications/FreeCAD.app/Contents/Resources/bin/freecadcmd past-experiments/case/stand_eink.py
Out:  past-experiments/case/export/stand_eink.stl (+ FCStd)

Print BASE DOWN, support-free. Same design language as the v1 FNK stand
(65-degree pocket wedge, open cable mouth): the device drops into an inclined
slot and leans back; a vertical mouth in the front lip lets the USB-C cable
exit without a tunnel or bridging.

Device envelope from Elecrow's official STEP model
(CrowPanel ESP32 4.2" E-paper HMI Display.stp): 109.60 x 87.30 x 9.80 mm.
The pocket takes the LONG edge, so the device can dock in either
orientation; at a 65-degree lean gravity keeps it centered.
"""

import os
import sys

import FreeCAD as App  # noqa: N813 — FreeCAD's own convention
import Part

# ---------------- measured / chosen ----------------
DEV_W = 109.60        # device long edge (STEP bbox)
DEV_T = 9.80          # device thickness (STEP bbox)
SLOT_CLR = 0.4        # per side
STAND_ANGLE = 65.0    # degrees from horizontal — v1-proven, not tippy
POCKET_DEPTH = 14.0   # how deep the edge sits
POCKET_Y = 22.0       # pocket center from the front face; long side aft
MOUTH_W = 16.0        # cable mouth width
MOUTH_FLOOR = 6.0     # mouth floor height — cable clears, lip keeps strength

POCKET_W = DEV_W + 2 * SLOT_CLR
POCKET_T = DEV_T + 2 * SLOT_CLR
BASE_W = POCKET_W + 12.0
BASE_D = 60.0
BASE_H = 20.0


def rounded_box(l, w, h, r):
    box = Part.makeBox(l, w, h)
    vertical = [e for e in box.Edges
                if abs(e.tangentAt(e.FirstParameter).z) > 0.99]
    return box.makeFillet(r, vertical)


def chamfer_at_z(solid, z_plane, size):
    edges = [e for e in solid.Edges
             if all(abs(v.Point.z - z_plane) < 1e-6 for v in e.Vertexes)]
    return solid.makeChamfer(size, edges)


def slot_shape(w, t, h):
    """An inclined box in stand coordinates — shared by the cut and the
    fit-proof mock so the two can never drift apart."""
    s = Part.makeBox(w, t, h)
    s.translate(App.Vector(-w / 2, -t / 2, 0))
    s.rotate(App.Vector(0, 0, 0), App.Vector(1, 0, 0), 90 - STAND_ANGLE)
    s.translate(App.Vector(BASE_W / 2, POCKET_Y, BASE_H - POCKET_DEPTH))
    return s


stand = rounded_box(BASE_W, BASE_D, BASE_H, 4.0)
stand = chamfer_at_z(stand, 0, 0.5)
stand = stand.cut(slot_shape(POCKET_W, POCKET_T, POCKET_DEPTH + 40))

# open cable mouth: vertical slot from the pocket floor out the front face
mouth = Part.makeBox(MOUTH_W, POCKET_Y + 0.5, BASE_H,
                     App.Vector((BASE_W - MOUTH_W) / 2, -0.5, MOUTH_FLOOR))
stand = stand.cut(mouth)

# ---------------- fit proof ----------------
# A nominal device (no clearance) sitting in the pocket must not touch the
# stand. Runs on every build — a parameter tweak that pinches the device
# fails here, not on the printer.
mock = slot_shape(DEV_W, DEV_T, 120)
clash = stand.common(mock).Volume
assert clash < 1e-3, f"device collides with stand: {clash:.2f} mm3"
print("fit proof: device clears the pocket")
assert stand.isValid(), "stand shape invalid"

# ---------------- document + export ----------------
out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "export")
os.makedirs(out_dir, exist_ok=True)
doc = App.newDocument("stand_eink")
obj = doc.addObject("Part::Feature", "stand_eink")
obj.Shape = stand
doc.recompute()

import Mesh  # noqa: E402 — FreeCAD module, importable only after init
Mesh.export([obj], os.path.join(out_dir, "stand_eink.stl"))
doc.saveAs(os.path.join(out_dir, "stand_eink.FCStd"))
print(f"exported: {os.path.join(out_dir, 'stand_eink.stl')}")
print(f"footprint: {BASE_W:.1f} x {BASE_D:.1f} x {BASE_H:.1f} mm, "
      f"pocket {POCKET_W:.1f} x {POCKET_T:.1f} @ {STAND_ANGLE:.0f} deg")
