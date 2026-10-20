#!/usr/bin/env python3
"""Build the install-guide artwork used by the landing page and the DMG PDF.

`1-dmg-window.png`, `2-drag.png` and `3-warning.png` are real screen captures
committed under `docs/assets/install/`. Only the Settings illustration
(`4-open-anyway.png`) is generated here by default; set
`TEXTLAYR_REDRAW_ALL=1` to rebuild the whole set (which replaces the captures
with compositions — useful only if the window design changes).

Run from the repo root:  python3 scripts/make_install_shots.py
"""

from __future__ import annotations

import os
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
OUT = ROOT / "docs" / "assets" / "install"

# 2x for crisp rendering on retina displays.
S = 2

INK = (27, 31, 36)
MUTED = (92, 102, 114)
BORDER = (214, 219, 226)
PANEL = (247, 248, 249)
WHITE = (255, 255, 255)
BLUE = (0, 122, 255)
RED = (229, 72, 77)

TITLEBAR = 28 * S
CONTENT_W, CONTENT_H = 640 * S, 520 * S


def font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont:
    name = "Inter-Bold.ttf" if bold else "Inter-Regular.ttf"
    return ImageFont.truetype(str(ASSETS / "fonts" / name), size * S)


def wrap(draw: ImageDraw.ImageDraw, text: str, f: ImageFont.FreeTypeFont, width: int) -> list[str]:
    words = text.split()
    lines: list[str] = []
    line = ""
    for word in words:
        trial = f"{line} {word}".strip()
        if draw.textlength(trial, font=f) <= width or not line:
            line = trial
        else:
            lines.append(line)
            line = word
    if line:
        lines.append(line)
    return lines


def icon_from_icns(path: Path, size: int) -> Image.Image:
    """Convert a system .icns to a transparent PNG-backed image."""
    with tempfile.TemporaryDirectory() as tmp:
        png = Path(tmp) / "icon.png"
        subprocess.run(
            ["sips", "-s", "format", "png", "--resampleHeightWidth", str(size), str(size), str(path), "--out", str(png)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return Image.open(png).convert("RGBA")


def rounded_shadow(canvas: Image.Image, box: tuple[int, int, int, int], radius: int) -> None:
    """Soft drop shadow behind a rounded rect, drawn onto `canvas`."""
    layer = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    d.rounded_rectangle(
        (box[0] + 4 * S, box[1] + 8 * S, box[2] + 4 * S, box[3] + 10 * S),
        radius=radius,
        fill=(0, 0, 0, 60),
    )
    layer = layer.filter(ImageFilter.GaussianBlur(9 * S))
    canvas.alpha_composite(layer)


def traffic_lights(d: ImageDraw.ImageDraw, x: int, y: int) -> None:
    for i, colour in enumerate(((255, 95, 87), (254, 188, 46), (40, 200, 64))):
        cx = x + i * 20 * S
        d.ellipse((cx - 6 * S, y - 6 * S, cx + 6 * S, y + 6 * S), fill=colour)


def dmg_window(with_drag: bool) -> Image.Image:
    w, h = CONTENT_W, CONTENT_H + TITLEBAR
    canvas = Image.new("RGBA", (w + 24 * S, h + 24 * S), (0, 0, 0, 0))
    x0, y0 = 12 * S, 10 * S
    box = (x0, y0, x0 + w, y0 + h)
    rounded_shadow(canvas, box, 12 * S)

    # Window body + title bar.
    d = ImageDraw.Draw(canvas)
    d.rounded_rectangle(box, radius=12 * S, fill=WHITE, outline=BORDER, width=1 * S)
    d.rounded_rectangle(
        (x0, y0, x0 + w, y0 + TITLEBAR),
        radius=12 * S,
        fill=(246, 246, 246),
    )
    d.rectangle((x0, y0 + TITLEBAR - 1 * S, x0 + w, y0 + TITLEBAR), fill=BORDER)
    traffic_lights(d, x0 + 20 * S, y0 + 14 * S)
    title_f = font(13, bold=True)
    tw = d.textlength("TextLayr", font=title_f)
    d.text((x0 + w / 2 - tw / 2, y0 + 7 * S), "TextLayr", font=title_f, fill=(74, 74, 74))

    # The real Finder background (arrow + instruction lines).
    bg = Image.open(ASSETS / "dmg-background.png").convert("RGB").resize((CONTENT_W, CONTENT_H), Image.LANCZOS)
    content = Image.new("RGBA", (CONTENT_W, CONTENT_H), (0, 0, 0, 0))
    content.paste(bg, (0, 0))
    canvas.alpha_composite(content, (x0, y0 + TITLEBAR))

    app_icon = Image.open(ASSETS / "icon.png").convert("RGBA").resize((128 * S, 128 * S), Image.LANCZOS)
    folder = icon_from_icns(
        Path("/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/ApplicationsFolderIcon.icns"),
        128 * S,
    )
    note = icon_from_icns(
        Path("/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns"),
        92 * S,
    )

    app_xy = (x0 + 160 * S, y0 + TITLEBAR + 170 * S)
    folder_xy = (x0 + 480 * S, y0 + TITLEBAR + 170 * S)
    note_xy = (x0 + 320 * S, y0 + TITLEBAR + 385 * S)

    if with_drag:
        # Ring the destination so the drop target is obvious.
        d.ellipse(
            (folder_xy[0] - 72 * S, folder_xy[1] - 72 * S, folder_xy[0] + 72 * S, folder_xy[1] + 72 * S),
            outline=RED,
            width=4 * S,
        )

    canvas.alpha_composite(app_icon, (app_xy[0] - 64 * S, app_xy[1] - 64 * S))
    canvas.alpha_composite(folder, (folder_xy[0] - 64 * S, folder_xy[1] - 64 * S))
    canvas.alpha_composite(note, (note_xy[0] - 46 * S, note_xy[1] - 46 * S))

    label_f = font(12)
    label = "Install guide.pdf"
    lw = ImageDraw.Draw(canvas).textlength(label, font=label_f)
    d.text((note_xy[0] - lw / 2, note_xy[1] + 50 * S), label, font=label_f, fill=(70, 70, 70))

    return canvas


def small_app_icon(size: int) -> Image.Image:
    """The bundle icon's small-size representation (the simplified monogram)."""
    return icon_from_icns(ASSETS / "AppIcon.icns", size)


def alert() -> Image.Image:
    w, h = 600 * S, 200 * S
    pad = 12 * S
    canvas = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    box = (pad, pad, pad + w, pad + h)
    rounded_shadow(canvas, box, 14 * S)

    d = ImageDraw.Draw(canvas)
    d.rounded_rectangle(box, radius=14 * S, fill=(242, 242, 242), outline=(210, 210, 210), width=1 * S)

    icon = small_app_icon(56 * S)
    canvas.alpha_composite(icon, (pad + 24 * S, pad + 24 * S))

    title_f = font(15, bold=True)
    body_f = font(12.5)
    d.text((pad + 96 * S, pad + 26 * S), "“TextLayr” Not Opened", font=title_f, fill=INK)

    body = (
        "Apple could not verify “TextLayr” is free of malware that may harm "
        "your Mac or compromise your privacy."
    )
    y = pad + 58 * S
    for line in wrap(d, body, body_f, w - 120 * S):
        d.text((pad + 96 * S, y), line, font=body_f, fill=INK)
        y += 17 * S

    # The call to action sits under the body, not outside the dialog.
    hint_f = font(12, bold=True)
    d.text((pad + 96 * S, pad + 116 * S), "Click Done — not Move to Trash", font=hint_f, fill=(190, 38, 44))

    # Buttons, bottom right. `Done` is the one to press.
    btn_f = font(12.5)
    done_label = "Done"
    trash_label = "Move to Trash"
    trash_w = int(d.textlength(trash_label, font=btn_f) + 26 * S)
    done_w = int(d.textlength(done_label, font=btn_f) + 30 * S)
    by = pad + h - 48 * S
    bx_done = pad + w - 20 * S - done_w
    bx_trash = bx_done - 12 * S - trash_w

    d.rounded_rectangle((bx_trash, by, bx_trash + trash_w, by + 30 * S), radius=7 * S, fill=WHITE, outline=(200, 200, 200), width=1 * S)
    d.text((bx_trash + 13 * S, by + 7 * S), trash_label, font=btn_f, fill=INK)
    d.rounded_rectangle((bx_done, by, bx_done + done_w, by + 30 * S), radius=7 * S, fill=BLUE)
    d.text((bx_done + 15 * S, by + 7 * S), done_label, font=btn_f, fill=WHITE)
    d.ellipse((bx_done - 7 * S, by - 7 * S, bx_done + done_w + 7 * S, by + 37 * S), outline=RED, width=4 * S)

    return canvas


def open_anyway() -> Image.Image:
    w, h = 680 * S, 310 * S
    pad = 12 * S
    canvas = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    box = (pad, pad, pad + w, pad + h)
    rounded_shadow(canvas, box, 14 * S)

    d = ImageDraw.Draw(canvas)
    d.rounded_rectangle(box, radius=14 * S, fill=WHITE, outline=BORDER, width=1 * S)
    # Window title bar.
    d.rounded_rectangle((pad, pad, pad + w, pad + 30 * S), radius=14 * S, fill=(246, 246, 246))
    d.rectangle((pad, pad + 29 * S, pad + w, pad + 30 * S), fill=BORDER)
    traffic_lights(d, pad + 18 * S, pad + 15 * S)
    head_f = font(13, bold=True)
    d.text((pad + 70 * S, pad + 8 * S), "Privacy & Security", font=head_f, fill=(60, 60, 60))

    section_f = font(13, bold=True)
    body_f = font(12)
    small_f = font(11.5)

    y = pad + 52 * S
    d.text((pad + 24 * S, y), "Security", font=section_f, fill=INK)
    y += 28 * S
    d.text((pad + 24 * S, y), "Allow applications from:", font=body_f, fill=MUTED)
    y += 22 * S
    d.rounded_rectangle((pad + 24 * S, y, pad + 24 * S + 300 * S, y + 26 * S), radius=6 * S, fill=PANEL, outline=BORDER, width=1 * S)
    d.text((pad + 34 * S, y + 5 * S), "App Store & Known Developers", font=small_f, fill=(90, 90, 90))
    y += 48 * S
    d.line((pad + 24 * S, y, pad + w - 24 * S, y), fill=BORDER, width=1 * S)
    y += 20 * S

    # The blocked-app row: icon + copy on the left, button on the right.
    btn_f = font(12.5)
    label = "Open Anyway"
    bw = int(d.textlength(label, font=btn_f) + 32 * S)
    bx = pad + w - 24 * S - bw
    by = y + 8 * S
    text_w = bx - (pad + 80 * S) - 18 * S

    icon = small_app_icon(44 * S)
    canvas.alpha_composite(icon, (pad + 24 * S, y))
    d.text((pad + 80 * S, y + 2 * S), "“TextLayr” was blocked from use", font=font(12.5, bold=True), fill=INK)
    lines = wrap(
        d,
        "because it is not from an identified developer. You can allow it to open anyway.",
        body_f,
        text_w,
    )
    ly = y + 22 * S
    for line in lines:
        d.text((pad + 80 * S, ly), line, font=body_f, fill=MUTED)
        ly += 17 * S

    d.rounded_rectangle((bx, by, bx + bw, by + 30 * S), radius=7 * S, fill=WHITE, outline=(190, 195, 202), width=1 * S)
    d.text((bx + 16 * S, by + 7 * S), label, font=btn_f, fill=INK)
    d.ellipse((bx - 7 * S, by - 7 * S, bx + bw + 7 * S, by + 37 * S), outline=RED, width=4 * S)

    note_f = font(11.5, bold=True)
    note = "click Open Anyway, then enter your admin password"
    nw = d.textlength(note, font=note_f)
    d.rounded_rectangle(
        (pad + 24 * S, pad + h - 42 * S, pad + 24 * S + nw + 20 * S, pad + h - 16 * S),
        radius=13 * S,
        fill=(253, 235, 236),
    )
    d.text((pad + 34 * S, pad + h - 37 * S), note, font=note_f, fill=(180, 35, 40))

    return canvas


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    # 1-dmg-window.png, 2-drag.png and 3-warning.png are real screen captures
    # (see docs/assets/install/). Only the Settings illustration is generated by
    # default, so running this after a capture session cannot replace them with
    # mockups. TEXTLAYR_REDRAW_ALL=1 rebuilds the whole set.
    shots = {"4-open-anyway.png": open_anyway()}
    if os.environ.get("TEXTLAYR_REDRAW_ALL") == "1":
        shots = {
            "1-dmg-window.png": dmg_window(with_drag=False),
            "2-drag.png": dmg_window(with_drag=True),
            "3-warning.png": alert(),
            **shots,
        }
    for name, im in shots.items():
        path = OUT / name
        im.save(path, optimize=True)
        print(f"wrote {path.relative_to(ROOT)} ({path.stat().st_size // 1024} KB, {im.width}x{im.height})")


if __name__ == "__main__":
    main()
