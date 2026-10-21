#!/usr/bin/env python3
"""Subset the app's bundled caption fonts for the landing-page style gallery.

The gallery renders each built-in caption profile over a still frame, so the
site needs the same families the app composites with: Nunito (Minimal) and
Libre Baskerville (Classic Serif, Editorial). Inter is already served for the
page itself.

The full TTFs live in the repo's `assets/fonts/` for the app; the site gets
ASCII-only WOFF subsets, which is a few tens of KB instead of half a megabyte.
Run this after changing a bundled font:

    python3 scripts/make_style_fonts.py

Heads up: the subset covers what the gallery can show, not arbitrary text. If
the preview copy grows beyond Latin-1 punctuation, widen UNICODES below.
"""

from __future__ import annotations

import pathlib
import sys

from fontTools import subset

REPO = pathlib.Path(__file__).resolve().parent.parent
SRC = REPO / "assets" / "fonts"
DST = REPO / "docs" / "assets" / "fonts"

# Printable ASCII, no-break space, en/em dash, curly quotes, ellipsis.
UNICODES = "U+0020-007E,U+00A0,U+2013-2014,U+2018-201D,U+2026"

FAMILIES = [
    ("Nunito-Regular.ttf", "Nunito-Regular.woff"),
    ("Nunito-Bold.ttf", "Nunito-Bold.woff"),
    ("LibreBaskerville-Regular.ttf", "LibreBaskerville-Regular.woff"),
    ("LibreBaskerville-Bold.ttf", "LibreBaskerville-Bold.woff"),
]


def main() -> int:
    if not SRC.is_dir():
        print(f"error: no fonts at {SRC}", file=sys.stderr)
        return 1
    DST.mkdir(parents=True, exist_ok=True)

    for src_name, out_name in FAMILIES:
        src = SRC / src_name
        out = DST / out_name
        if not src.is_file():
            print(f"error: missing {src}", file=sys.stderr)
            return 1
        subset.main([
            str(src),
            f"--unicodes={UNICODES}",
            "--flavor=woff",
            "--no-hinting",
            "--desubroutinize",
            f"--output-file={out}",
        ])
        print(f"    {out.relative_to(REPO)}  {out.stat().st_size // 1024} KB")

    # The site serves these files, so the licence has to travel with them.
    licence = SRC / "OFL.txt"
    if licence.is_file():
        (DST / "OFL.txt").write_bytes(licence.read_bytes())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
