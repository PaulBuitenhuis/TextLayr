#!/usr/bin/env python3
"""Build the one-page-per-screen PDF install guide that ships inside the DMG.

It reuses the artwork from scripts/make_install_shots.py, so the website, the
DMG and this PDF cannot drift apart.

Output: docs/assets/install/TextLayr-install-guide.pdf

Run from the repo root:  python3 scripts/make_install_guide.py
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
SHOTS = ROOT / "docs" / "assets" / "install"
OUT = SHOTS / "TextLayr-install-guide.pdf"

DPI = 150
W, H = int(8.27 * DPI), int(11.69 * DPI)  # A4
MARGIN = 86
CW = W - MARGIN * 2

INK = (27, 31, 36)
MUTED = (92, 102, 114)
ACCENT = (47, 128, 255)
RED = (190, 38, 44)
RULE = (222, 226, 232)
PANEL = (246, 248, 250)


def font(size: float, bold: bool = False) -> ImageFont.FreeTypeFont:
    name = "Inter-Bold.ttf" if bold else "Inter-Regular.ttf"
    return ImageFont.truetype(str(ASSETS / "fonts" / name), int(size * DPI / 72))


def wrap(d: ImageDraw.ImageDraw, text: str, f: ImageFont.FreeTypeFont, width: int) -> list[str]:
    words = text.split()
    lines: list[str] = []
    line = ""
    for word in words:
        trial = f"{line} {word}".strip()
        if d.textlength(trial, font=f) <= width or not line:
            line = trial
        else:
            lines.append(line)
            line = word
    if line:
        lines.append(line)
    return lines


def para(
    d: ImageDraw.ImageDraw,
    x: int,
    y: int,
    text: str,
    f: ImageFont.FreeTypeFont,
    width: int = CW,
    fill=INK,
    leading: float = 1.45,
) -> int:
    lh = int(f.size * leading)
    for line in wrap(d, text, f, width):
        d.text((x, y), line, font=f, fill=fill)
        y += lh
    return y


def step(d, n: int, title: str, y: int) -> int:
    r = 13
    d.ellipse((MARGIN, y + 2, MARGIN + r * 2, y + 2 + r * 2), fill=INK)
    f = font(10.5, bold=True)
    tw = d.textlength(str(n), font=f)
    d.text((MARGIN + r - tw / 2, y + 5), str(n), font=f, fill=(255, 255, 255))
    d.text((MARGIN + r * 2 + 12, y), title, font=font(14, bold=True), fill=INK)
    return y + 42


def code_box(d, y: int, text: str) -> int:
    f = font(9.5)
    pad = 12
    d.rounded_rectangle(
        (MARGIN, y, MARGIN + CW, y + 40),
        radius=7,
        fill=PANEL,
        outline=RULE,
    )
    d.text((MARGIN + pad, y + 13), text, font=f, fill=INK)
    return y + 54


def page() -> Image.Image:
    return Image.new("RGB", (W, H), (255, 255, 255))


def footer(d, page_no: int) -> None:
    f = font(8.5)
    d.text((MARGIN, H - 26), "TextLayr · paulbuitenhuis.github.io/TextLayr", font=f, fill=MUTED)
    label = f"Page {page_no} of 2"
    tw = d.textlength(label, font=f)
    d.text((W - MARGIN - tw, H - 26), label, font=f, fill=MUTED)


def page_one() -> Image.Image:
    im = page()
    d = ImageDraw.Draw(im)
    y = 64

    banner = Image.open(ASSETS / "banner-light.png").convert("RGBA")
    bw = 260
    bh = int(banner.height * bw / banner.width)
    banner = banner.resize((bw, bh), Image.LANCZOS)
    im.paste(banner, (MARGIN, y), banner)
    y += bh + 26

    d.text((MARGIN, y), "Install guide", font=font(20, bold=True), fill=INK)
    y += 62
    y = para(
        d,
        MARGIN,
        y,
        "Requires macOS 13 or later on an Apple Silicon Mac. The download is about "
        "8 MB; the first launch fetches ffmpeg and a Whisper model (~190 MB).",
        font(10.5),
        fill=MUTED,
    )
    y += 18

    # At-a-glance requirements, so page 1 answers the obvious questions.
    box_h = 96
    d.rounded_rectangle((MARGIN, y, W - MARGIN, y + box_h), radius=8, fill=PANEL, outline=RULE)
    bx = MARGIN + 18
    by = y + 16
    checks = [
        "macOS 13 (Ventura) or later, on an Apple Silicon (M-series) Mac",
        "An internet connection for the one-time setup download",
        "About 250 MB of free disk space (~600 MB with the small.en model)",
    ]
    for item in checks:
        d.ellipse((bx, by + 4, bx + 8, by + 12), fill=ACCENT)
        d.text((bx + 18, by), item, font=font(10), fill=INK)
        by += 22
    y += box_h + 24

    d.line((MARGIN, y, W - MARGIN, y), fill=RULE)
    y += 22

    y = step(d, 1, "Open the disk image and drag TextLayr to Applications", y)
    y = para(
        d,
        MARGIN + 38,
        y,
        "Double-click the TextLayr disk image in your Downloads folder. Drag TextLayr "
        "onto the Applications shortcut, then eject the disk image. Do this before "
        "you open the app: running it from the image makes macOS use a temporary "
        "copy that cannot update itself.",
        font(10.5),
    )
    y += 8

    shot = Image.open(SHOTS / "2-drag.png").convert("RGBA")
    sw = CW - 20
    sh = int(shot.height * sw / shot.width)
    shot = shot.resize((sw, sh), Image.LANCZOS)
    im.paste(shot, (MARGIN + 20, y), shot)
    y += sh + 18

    d.text(
        (MARGIN + 38, y),
        "The Install guide in the window has the same one-time fix as page 2.",
        font=font(9.5),
        fill=MUTED,
    )

    footer(d, 1)
    return im


def page_two() -> Image.Image:
    im = page()
    d = ImageDraw.Draw(im)
    y = 64

    d.text((MARGIN, y), "Through the macOS warning", font=font(18, bold=True), fill=INK)
    y += 54

    y = step(d, 2, "Open it from Applications and click Done", y)
    y = para(
        d,
        MARGIN + 38,
        y,
        "macOS refuses the app and offers to move it to the Trash. Click Done — not "
        "Move to Trash (some Macs label it Move to Bin). TextLayr is fine; macOS just "
        "cannot verify an app that is not notarized. The exact wording varies by macOS "
        "version.",
        font(10.5),
    )
    y += 6

    shot = Image.open(SHOTS / "3-warning.png").convert("RGBA")
    sw = 480
    sh = int(shot.height * sw / shot.width)
    shot = shot.resize((sw, sh), Image.LANCZOS)
    im.paste(shot, (MARGIN + (CW - sw) // 2, y), shot)
    y += sh + 14

    y = step(d, 3, "Allow it once in System Settings", y)
    y = para(
        d,
        MARGIN + 38,
        y,
        "Open System Settings → Privacy & Security and scroll to Security. Click "
        "Open Anyway next to the message about TextLayr, confirm with your admin "
        "password, and open the app again. The button only appears for about an hour "
        "after the blocked attempt. No button? Clear the flag in Terminal:",
        font(10.5),
    )
    y += 2
    y = code_box(d, y, "xattr -dr com.apple.quarantine /Applications/TextLayr.app")
    y += 2

    shot = Image.open(SHOTS / "4-open-anyway.png").convert("RGBA")
    sw = CW - 70
    sh = int(shot.height * sw / shot.width)
    shot = shot.resize((sw, sh), Image.LANCZOS)
    im.paste(shot, (MARGIN + 35, y), shot)
    y += sh + 20

    y = step(d, 4, "Finish setup", y)
    y = para(
        d,
        MARGIN + 38,
        y,
        "Choose base.en (fast, ~148 MB) or small.en (more accurate, ~488 MB), then "
        "click Install. ffmpeg and the model are downloaded once; after that TextLayr "
        "works offline. No account, no uploads.",
        font(10.5),
        width=CW - 60,
    )
    y += 12

    d.line((MARGIN, y, W - MARGIN, y), fill=RULE)
    y += 18
    d.text((MARGIN, y), "Prefer no security prompt at all?", font=font(11, bold=True), fill=INK)
    y += 22
    y = para(
        d,
        MARGIN,
        y,
        "Terminal install: files fetched with curl never get the quarantine flag.",
        font(9.5),
        fill=MUTED,
    )
    y += 2
    y = code_box(d, y, "curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh")

    d.text((MARGIN, y), "Uninstall", font=font(11, bold=True), fill=INK)
    y += 22
    para(
        d,
        MARGIN,
        y,
        "Drag TextLayr to the Trash, then remove its downloads in Settings → Diagnostics.",
        font(9.5),
        fill=MUTED,
    )

    footer(d, 2)
    return im


def main() -> None:
    pages = [page_one(), page_two()]
    pages[0].save(
        OUT,
        "PDF",
        resolution=float(DPI),
        save_all=True,
        append_images=pages[1:],
    )
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
