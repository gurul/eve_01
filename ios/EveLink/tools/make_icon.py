"""Draws Eve Link's app icon (1024 px, no alpha, iOS masks the corners): black, with eve's two white pill eyes.
Run: python3 tools/make_icon.py OUT.png
The owner asked for a logo on 2026-10-04 ("give the app a logo, its ugly"), and on 2026-10-08 for a minimal one:
"just white eyes"."""
import sys

from PIL import Image, ImageDraw

S = 1024
SS = 4                        # drawn 4x and scaled down: smooth edges without anti-aliasing support in ImageDraw
W = S * SS
EYE_W, EYE_H, GAP = 220, 110, 92   # one eye's width and height, and the gap between them, in icon pixels


def main(out: str) -> None:
    img = Image.new("RGB", (W, W), (0, 0, 0))
    d = ImageDraw.Draw(img)
    cy = W // 2
    for x in (W // 2 - (GAP // 2 + EYE_W) * SS, W // 2 + GAP // 2 * SS):
        d.rounded_rectangle([x, cy - EYE_H // 2 * SS, x + EYE_W * SS, cy + EYE_H // 2 * SS],
                            radius=EYE_H // 2 * SS, fill=(255, 255, 255))
    img.resize((S, S), Image.LANCZOS).save(out)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "icon-1024.png")
