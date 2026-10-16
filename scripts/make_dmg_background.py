#!/usr/bin/env python3
"""Generate the DMG background art.

Writes assets/dmg-background.png, sized to match the install window in
scripts/make_dmg.sh (W x H). Kept in the repo so packaging doesn't need Python.

    scripts/make_dmg_background.py
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

# Must match scripts/make_dmg.sh.
W, H = 640, 400
APP_X, APP_Y = 160, 180
LINK_X, LINK_Y = 480, 180

BG = (246, 246, 248)
ARROW = (183, 183, 191)
TEXT = (138, 138, 146)


def main() -> int:
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)

    # Arrow across the gap, with clear space before the Applications folder
    # (which spans roughly LINK_X +/- 64).
    y = APP_Y - 4
    d.rounded_rectangle((APP_X + 80, y - 5, LINK_X - 114, y + 5), radius=5, fill=ARROW)
    base = LINK_X - 114
    d.polygon([(base, y - 19), (base, y + 19), (base + 34, y)], fill=ARROW)

    try:
        font = ImageFont.truetype(str(ASSETS / "fonts" / "Inter-Regular.ttf"), 15)
    except OSError:
        font = ImageFont.load_default()
    d.text((W // 2, 300), "Drag texxy to Applications", font=font, fill=TEXT, anchor="mm")

    out = ASSETS / "dmg-background.png"
    img.save(out)
    print(f"wrote {out.relative_to(ROOT)} ({out.stat().st_size // 1024} KiB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
