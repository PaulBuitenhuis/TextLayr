#!/usr/bin/env python3
"""Generate TextLayr's application icon.

Produces:
  assets/icon.png     1024x1024 wordmark, used at runtime for the dock icon
  assets/icon-mark.png  1024x1024 "T" monogram, for places too small to read
                        the wordmark (favicons, site header/footer)
  assets/AppIcon.icns macOS bundle icon (referenced by Info.plist)
  assets/AppIcon.ico  Windows icon (multi-resolution)
  assets/icons/hicolor/<N>x<N>/apps/textlayr.png
                      Linux icon-theme PNGs (16…256px)
  assets/banner-{dark,light}.png  transparent wordmark lockup for the app
                                   header and the README (white ink for dark
                                   backgrounds, dark ink for light ones)

The mark is the word "TextLayr" in white (Inter, the same family the app can
caption with) sitting on an accent-blue highlight bar, with the descender of
the "y" ending on the bar's bottom edge — the karaoke highlight made a logo.

The .icns is **adaptive**: the 16px and 32px slots hold a simplified "t"
monogram instead of the wordmark, which is unreadable that small. (Finder and
Dock use the per-size representation; only the runtime bitmap is fixed-size.)

The same monogram is written to assets/icon-mark.png so that small placements —
the site header and footer, and the favicon — can use it too.

Note: the website keeps its own copies under docs/assets/ (icon.png, mark.png,
banner-*.png, favicon.png). Re-run this script and copy them over after
changing the mark:

Requires Pillow + numpy, and macOS `iconutil` for the .icns step:

    scripts/make_icon.py
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

SIZE = 1024
SS = 4  # supersample factor
C = SIZE * SS  # canvas in supersampled pixels

# Palette.
BG_TOP = (40, 44, 54, 255)
BG_BOTTOM = (15, 17, 22, 255)
ACCENT = (10, 132, 255, 255)  # theme::ACCENT
WHITE = (247, 249, 252, 255)
DARK = (28, 31, 38, 255)  # wordmark ink for the light-background banner

# Wordmark geometry, in 1024-space.
TEXT = "TextLayr"
TEXT_WIDTH = 780  # near the width of the squircle's flat area
RULE_H = 84  # highlight bar height
RULE_PAD = 8  # how far the bar extends past the word on each side
NUDGE_Y = -18  # optical centring of the whole lockup

# Small-mark geometry: a single "t" large enough to read at 16px.
SMALL_GLYPH = "T"
SMALL_HEIGHT = 700
SMALL_NUDGE_Y = -10

# Banner geometry: the transparent wordmark lockup used in the app header and
# the README (no squircle background, so it works on any page colour).
BANNER_H = 160  # output pixel height (displayed ~22px, so generously oversampled)

FONT_PATH = ASSETS / "fonts" / "Inter-Bold.ttf"


def squircle_bg() -> Image.Image:
    img = Image.new("RGBA", (C, C), (0, 0, 0, 0))
    pad = 72 * SS
    radius = int(0.2237 * (SIZE - 144) * SS)
    t = np.array(BG_TOP, dtype=np.float32)
    b = np.array(BG_BOTTOM, dtype=np.float32)
    ramp = np.linspace(0.0, 1.0, C, dtype=np.float32)[:, None]
    cols = np.repeat((t[None, :] * (1 - ramp) + b[None, :] * ramp)[:, None, :], C, axis=1)
    mask = Image.new("L", (C, C), 0)
    ImageDraw.Draw(mask).rounded_rectangle((pad, pad, C - pad, C - pad), radius=radius, fill=255)
    img.paste(Image.fromarray(cols.astype(np.uint8)), (0, 0), mask)
    return img


def rrect(img: Image.Image, box, radius: int, fill) -> None:
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).rounded_rectangle(box, radius=radius, fill=fill)
    img.alpha_composite(layer)


def fit(text: str, target_w: int | None = None, target_h: int | None = None) -> ImageFont.FreeTypeFont:
    """Largest Inter-Bold size fitting `text` within a width and/or height."""
    lo, hi = 10, 4000
    best = ImageFont.truetype(str(FONT_PATH), lo)
    for _ in range(48):
        mid = (lo + hi) // 2
        font = ImageFont.truetype(str(FONT_PATH), mid)
        box = font.getbbox(text)
        w, h = box[2] - box[0], box[3] - box[1]
        if (target_w is None or w <= target_w) and (target_h is None or h <= target_h):
            best, lo = font, mid + 1
        else:
            hi = mid - 1
    return best


def text_mask(text: str, font: ImageFont.FreeTypeFont, y: int) -> Image.Image:
    mask = Image.new("L", (C, C), 0)
    ImageDraw.Draw(mask).text((C // 2, y), text, font=font, fill=255, anchor="mm")
    return mask


def ink_bounds(mask: Image.Image) -> tuple[int, int, int, int]:
    """(left, top, right, bottom) of the mask's non-zero pixels."""
    arr = np.array(mask)
    rows = np.where(arr.any(axis=1))[0]
    cols = np.where(arr.any(axis=0))[0]
    return int(cols[0]), int(rows[0]), int(cols[-1]), int(rows[-1])


def mono(level: int) -> Image.Image:
    return Image.new("L", (C, C), level)


def paint(img: Image.Image, mask: Image.Image, rgb) -> None:
    img.alpha_composite(
        Image.merge("RGBA", (mono(rgb[0]), mono(rgb[1]), mono(rgb[2]), mask))
    )


def paint_white(img: Image.Image, mask: Image.Image) -> None:
    paint(img, mask, WHITE)


def centered_y(text: str, font: ImageFont.FreeTypeFont, nudge: int) -> int:
    """A baseline-y that centres the glyph's ink, plus an optical nudge."""
    _, top, _, bottom = ink_bounds(text_mask(text, font, C // 2))
    return C // 2 - ((top + bottom) // 2 - C // 2) + nudge * SS


def build() -> Image.Image:
    """Full wordmark (used for the 64px-and-up slots and the runtime bitmap)."""
    img = squircle_bg()
    font = fit(TEXT, target_w=TEXT_WIDTH * SS)
    y = centered_y(TEXT, font, NUDGE_Y)
    mask = text_mask(TEXT, font, y)
    left, _, right, bottom = ink_bounds(mask)

    # Highlight bar: spans the word, its bottom on the y's descender.
    rule_h = RULE_H * SS
    rrect(img, (left - RULE_PAD * SS, bottom - rule_h, right + RULE_PAD * SS, bottom), rule_h // 2, ACCENT)
    paint_white(img, mask)
    return img.resize((SIZE, SIZE), Image.LANCZOS)


def build_small() -> Image.Image:
    """Simplified "T" monogram for the tiny icon slots."""
    img = squircle_bg()
    font = fit(SMALL_GLYPH, target_h=SMALL_HEIGHT * SS)
    paint_white(img, text_mask(SMALL_GLYPH, font, centered_y(SMALL_GLYPH, font, SMALL_NUDGE_Y)))
    return img.resize((SIZE, SIZE), Image.LANCZOS)


def build_banner(fg) -> Image.Image:
    """Transparent wordmark lockup: "TextLayr" over the accent highlight bar,
    with no background so it sits on the app header or a README of any colour.
    Cropped tight and scaled to BANNER_H."""
    img = Image.new("RGBA", (C, C), (0, 0, 0, 0))
    font = fit(TEXT, target_w=TEXT_WIDTH * SS)
    y = centered_y(TEXT, font, NUDGE_Y)
    mask = text_mask(TEXT, font, y)
    left, top, right, bottom = ink_bounds(mask)

    rule_h = RULE_H * SS
    bar = (left - RULE_PAD * SS, bottom - rule_h, right + RULE_PAD * SS, bottom)
    rrect(img, bar, rule_h // 2, ACCENT)
    paint(img, mask, fg)

    pad = 6 * SS
    box = (
        max(min(left, bar[0]) - pad, 0),
        max(min(top, bar[1]) - pad, 0),
        min(max(right, bar[2]) + pad, C),
        min(max(bottom, bar[3]) + pad, C),
    )
    img = img.crop(box)
    scale = BANNER_H / img.height
    return img.resize((max(1, round(img.width * scale)), BANNER_H), Image.LANCZOS)


def write_icns(wordmark: Image.Image, small: Image.Image) -> None:
    iconset = ASSETS / "AppIcon.iconset"
    if iconset.exists():
        shutil.rmtree(iconset)
    iconset.mkdir()

    # (pixel size, name, source): tiny slots get the monogram, the rest the wordmark.
    specs = [
        (16, "icon_16x16.png", small),
        (32, "icon_16x16@2x.png", small),
        (32, "icon_32x32.png", small),
        (64, "icon_32x32@2x.png", wordmark),
        (128, "icon_128x128.png", wordmark),
        (256, "icon_128x128@2x.png", wordmark),
        (256, "icon_256x256.png", wordmark),
        (512, "icon_256x256@2x.png", wordmark),
        (512, "icon_512x512.png", wordmark),
        (1024, "icon_512x512@2x.png", wordmark),
    ]
    for px, name, src in specs:
        src.resize((px, px), Image.LANCZOS).save(iconset / name)

    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset), "-o", str(ASSETS / "AppIcon.icns")],
        check=True,
    )
    shutil.rmtree(iconset)


def write_ico(wordmark: Image.Image) -> None:
    """Windows multi-resolution `.ico` (256…16px)."""
    ico = ASSETS / "AppIcon.ico"
    wordmark.save(
        ico,
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print(f"wrote {ico.relative_to(ROOT)} ({ico.stat().st_size // 1024} KiB)")


def write_hicolor(wordmark: Image.Image, small: Image.Image) -> None:
    """Linux icon-theme (hicolor) PNG set; tiny sizes use the monogram."""
    base = ASSETS / "icons" / "hicolor"
    for px in (16, 32, 48, 64, 128, 256):
        src = small if px <= 32 else wordmark
        out_dir = base / f"{px}x{px}" / "apps"
        out_dir.mkdir(parents=True, exist_ok=True)
        src.resize((px, px), Image.LANCZOS).save(out_dir / "textlayr.png")
    print(f"wrote {base.relative_to(ROOT)}/{{16,32,48,64,128,256}}x.../apps/textlayr.png")


def main() -> int:
    ASSETS.mkdir(exist_ok=True)
    wordmark = build()
    png = ASSETS / "icon.png"
    wordmark.save(png)
    print(f"wrote {png.relative_to(ROOT)} ({png.stat().st_size // 1024} KiB)")

    # Same monogram the .icns uses for its tiny slots, as a standalone asset so
    # small placements (favicon, site header/footer) show a legible "T" instead
    # of an unreadable smudge of the wordmark.
    small = build_small()
    mark_png = ASSETS / "icon-mark.png"
    small.save(mark_png)
    print(f"wrote {mark_png.relative_to(ROOT)} ({mark_png.stat().st_size // 1024} KiB)")

    for name, fg in (("banner-dark.png", WHITE), ("banner-light.png", DARK)):
        out = ASSETS / name
        build_banner(fg).save(out)
        print(f"wrote {out.relative_to(ROOT)} ({out.stat().st_size // 1024} KiB)")

    # Windows + Linux icons (platform-independent; no macOS tooling needed).
    write_ico(wordmark)
    write_hicolor(wordmark, small)

    if shutil.which("iconutil") is None:
        print("iconutil not found — skipping .icns (macOS only)", file=sys.stderr)
        return 0
    write_icns(wordmark, small)
    print(f"wrote {(ASSETS / 'AppIcon.icns').relative_to(ROOT)} (adaptive: T monogram at 16/32px)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
