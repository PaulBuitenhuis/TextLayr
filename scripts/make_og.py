#!/usr/bin/env python3
"""Build docs/assets/og.jpg — the 1200x630 social preview card.

It reuses the committed dark banner wordmark, so the card matches the site hero
and cannot drift from it. Re-run after changing the banner or the platform line.

Run from the repo root:  python3 scripts/make_og.py
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
OUT = ROOT / "docs" / "assets" / "og.jpg"

W, H = 1200, 630
BG = (14, 16, 20)
TEXT = (240, 243, 247)
MUTED = (150, 158, 170)

SUBTITLE = "Word-synced karaoke captions, burned in offline."
PLATFORMS = "macOS & Linux · Free & open source · No uploads"


def font(size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(ASSETS / "fonts" / "Inter-Regular.ttf"), size)


def main() -> None:
    card = Image.new("RGB", (W, H), BG)

    banner = Image.open(ASSETS / "banner-dark.png").convert("RGBA")
    banner_w = 620
    banner_h = round(banner.height * banner_w / banner.width)
    banner = banner.resize((banner_w, banner_h), Image.Resampling.LANCZOS)
    card.paste(banner, ((W - banner_w) // 2, 155), banner)

    draw = ImageDraw.Draw(card)
    draw.text((W // 2, 390), SUBTITLE, font=font(38), fill=TEXT, anchor="ma")
    draw.text((W // 2, 455), PLATFORMS, font=font(23), fill=MUTED, anchor="ma")

    OUT.parent.mkdir(parents=True, exist_ok=True)
    card.save(OUT, "JPEG", quality=92, optimize=True)
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
