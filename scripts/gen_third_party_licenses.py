#!/usr/bin/env python3
"""Regenerate THIRD_PARTY_LICENSES.md from the resolved dependency graph.

TextLayr statically links ~400 Rust crates, plus it bundles the caption fonts and a
copy of whisper.cpp. MIT/Apache-2.0/BSD/Zlib etc. all require that the copyright
notice and license text travel with a binary distribution, so this script walks
`cargo metadata`, collects every crate's shipped license/notice files, and emits
one deduplicated document.

Run it from the crate root after changing dependencies:

    scripts/gen_third_party_licenses.py

The output is deterministic (sorted by crate name, license texts deduplicated by
content and ordered by first appearance), so a re-run with no dependency changes
produces an identical file.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

CRATE_ROOT = Path(__file__).resolve().parent.parent
OUT = CRATE_ROOT / "THIRD_PARTY_LICENSES.md"

# Files worth pulling from a crate's source directory (case-insensitive, root
# level only). Covers the common conventions across the ecosystem.
LICENSE_FILE_RE = re.compile(
    r"^(licen[cs]e|copying|notice|unlicense|copyright)",
    re.IGNORECASE,
)

# Repo-relative files for components that are bundled/compiled but are not cargo
# packages, so their texts must be embedded by hand.
BUNDLED = [
    ("Caption fonts (Inter, Nunito, Libre Baskerville)", "assets/fonts/OFL.txt"),
    (
        "whisper.cpp",
        "vendor/patch/whisper-rs-sys/whisper.cpp/LICENSE",
    ),
]


def load_metadata() -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        cwd=CRATE_ROOT,
        check=True,
        capture_output=True,
    ).stdout
    return json.loads(out)


def normal_dependency_ids(meta: dict, root_name: str) -> set[str]:
    """IDs of everything reachable from the root through *normal* deps only
    (dev- and build-only crates never end up in the shipped binary)."""
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p for p in meta["packages"] if p["name"] == root_name)

    seen: set[str] = set()
    stack = list(nodes[root["id"]]["deps"])
    while stack:
        dep = stack.pop()
        kinds = {dk.get("kind") or "normal" for dk in dep.get("dep_kinds", [])}
        if "normal" not in kinds:
            continue
        pid = dep["pkg"]
        if pid in seen:
            continue
        seen.add(pid)
        stack.extend(nodes[pid]["deps"])
    return seen


def read_text(path: Path) -> str | None:
    try:
        raw = path.read_bytes()
    except OSError:
        return None
    # Strip a UTF-8 BOM and normalise newlines; skip anything that smells binary.
    if b"\x00" in raw[:4096]:
        return None
    text = raw.decode("utf-8", errors="replace").lstrip("\ufeff")
    return text.replace("\r\n", "\n").replace("\r", "\n").strip() + "\n"


def collect_license_files(pkg: dict) -> list[tuple[str, str]]:
    """Return (filename, text) pairs declared or conventionally present."""
    manifest = Path(pkg["manifest_path"])
    src_dir = manifest.parent
    found: list[tuple[str, str]] = []
    seen_names: set[str] = set()

    declared = pkg.get("license_file")
    if declared:
        p = src_dir / declared
        text = read_text(p)
        if text:
            found.append((Path(declared).name, text))
            seen_names.add(Path(declared).name.lower())

    try:
        entries = sorted(src_dir.iterdir(), key=lambda p: p.name.lower())
    except OSError:
        entries = []
    for entry in entries:
        if not entry.is_file():
            continue
        if entry.name.lower() in seen_names:
            continue
        if LICENSE_FILE_RE.match(entry.name):
            text = read_text(entry)
            if text:
                found.append((entry.name, text))
                seen_names.add(entry.name.lower())
    return found


def main() -> int:
    meta = load_metadata()
    root_name = next(p["name"] for p in meta["packages"] if p["name"] == "textlayr")
    wanted = normal_dependency_ids(meta, root_name)

    packages = sorted(
        (p for p in meta["packages"] if p["id"] in wanted),
        key=lambda p: (p["name"].lower(), p["version"]),
    )

    # Deduplicate identical license texts, remembering which crates they cover.
    text_to_id: dict[str, str] = {}
    sections: dict[str, dict] = {}
    for pkg in packages:
        for fname, text in collect_license_files(pkg):
            digest = hashlib.sha1(text.encode("utf-8")).hexdigest()
            key = text_to_id.setdefault(digest, digest)
            sec = sections.setdefault(key, {"text": text, "crates": [], "files": set()})
            label = f"{pkg['name']} {pkg['version']}"
            if label not in sec["crates"]:
                sec["crates"].append(label)
            sec["files"].add(fname)

    order = list(sections.keys())

    lines: list[str] = []
    a = lines.append
    a("# Third-party licenses")
    a("")
    a(
        "TextLayr is released under the MIT License (see [LICENSE](./LICENSE)). It "
        "builds on the work below; this file reproduces the notices and license "
        "texts those projects require a redistributed binary to carry."
    )
    a("")
    a("> Generated by `scripts/gen_third_party_licenses.py`. Do not edit by hand.")
    a("")
    a("## Bundled at build time")
    a("")
    a("These ship inside `TextLayr.app` (or, for whisper.cpp, are compiled into the binary):")
    a("")
    a("- **Caption fonts** — Inter, Nunito and Libre Baskerville, each under the "
      "SIL Open Font License 1.1.")
    a("- **whisper.cpp** — MIT License, (c) 2023-2024 The ggml authors. Compiled "
      "in via `whisper-rs`.")
    a("")
    for name, rel in BUNDLED:
        text = read_text(CRATE_ROOT / rel)
        if not text:
            print(f"warning: bundled license not found: {rel}", file=sys.stderr)
            continue
        a(f"### {name} (`{rel}`)")
        a("")
        a("```text")
        a(text.rstrip("\n"))
        a("```")
        a("")
    a("## Not bundled")
    a("")
    a(
        "`ffmpeg` and `ffprobe` are **not** distributed with TextLayr. They are either "
        "found on your `PATH`, installed via Homebrew, or downloaded by you on first "
        "run. The whisper model (`ggml-*.bin`) is likewise downloaded on demand. "
        "Those components carry their own licenses, which you accept when you obtain "
        "them; TextLayr merely invokes them as separate programs."
    )
    a("")
    a(f"## Dependency inventory ({len(packages)} crates)")
    a("")
    a("| Crate | Version | License (SPDX) |")
    a("|---|---|---|")
    for pkg in packages:
        lic = pkg.get("license") or "see text below"
        a(f"| {pkg['name']} | {pkg['version']} | {lic} |")
    a("")

    if order:
        a(f"## License texts ({len(order)} distinct)")
        a("")
        for idx, key in enumerate(order, start=1):
            sec = sections[key]
            crates = sorted(sec["crates"])
            files = ", ".join(sorted(sec["files"]))
            a(f"### {idx}. Applies to {len(crates)} crate(s) — `{files}`")
            a("")
            for c in crates:
                a(f"- {c}")
            a("")
            a("```text")
            a(sec["text"].rstrip("\n"))
            a("```")
            a("")

    OUT.write_text("\n".join(lines) + "\n", encoding="utf-8")
    size_kb = OUT.stat().st_size / 1024
    print(
        f"wrote {OUT.relative_to(CRATE_ROOT)} "
        f"({len(packages)} crates, {len(order)} license texts, {size_kb:.0f} KiB)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
