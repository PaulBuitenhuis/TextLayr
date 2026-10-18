#!/usr/bin/env bash
#
# Build a self-contained TextLayr.app for macOS (Apple Silicon).
#
# The bundle contains the release binary and the caption fonts. It does NOT
# contain ffmpeg/ffprobe or the whisper model: on first run TextLayr downloads
# those into ~/Library/Application Support/textlayr/ (see the setup screen, or
# `textlayr --setup`).
#
# Output:
#   dist/TextLayr.app
#   dist/TextLayr-<version>.zip      (zip of the .app)
#   dist/TextLayr-<version>.dmg      (drag-to-Applications disk image)
#
# Usage:
#   scripts/package_macos.sh
#
# Inputs (each can be overridden by an environment variable):
#   TEXTLAYR_VERSION     version string (default: `git describe --tags`, else the
#                     version in Cargo.toml)
#   CODESIGN_IDENTITY codesign identity; "-" = ad-hoc (default). TextLayr is not
#                     notarized, so a build that reaches a user's Mac as a browser
#                     download is refused by Gatekeeper. `docs/install.sh` (curl,
#                     so never quarantined) or the one-time "Open Anyway"/`xattr`
#                     route from the README are the ways through it.
#   SKIP_ARCHIVES=1   build only dist/TextLayr.app (no zip/dmg)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

app="$repo_root/dist/TextLayr.app"
contents="$app/Contents"
dist="$repo_root/dist"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ── 1. release build ────────────────────────────────────────────────────────
say "building release binary"
cargo build --release

binary="$repo_root/target/release/textlayr"
[[ -x "$binary" ]] || die "release binary not found at $binary"

# ── 2. locate the pieces to bundle ──────────────────────────────────────────
fonts_src="$repo_root/assets/fonts"
[[ -d "$fonts_src" ]] || die "bundled fonts not found at $fonts_src"

version="${TEXTLAYR_VERSION:-}"
if [[ -z "$version" ]]; then
    version="$(git describe --tags --always 2>/dev/null || true)"
fi
if [[ -z "$version" ]]; then
    version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
fi
[[ -n "$version" ]] || die "could not determine a version (set TEXTLAYR_VERSION)"

# ── 3. assemble the bundle ──────────────────────────────────────────────────
say "assembling $app"
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources/assets/fonts"

cp "$binary" "$contents/MacOS/TextLayr"
chmod +x "$contents/MacOS/TextLayr"

# Fonts plus their OFL license (the license must travel with the fonts).
cp "$fonts_src"/*.ttf "$contents/Resources/assets/fonts/"
[[ -f "$fonts_src/OFL.txt" ]] && cp "$fonts_src/OFL.txt" "$contents/Resources/assets/fonts/"

# Application icon.
icon="$repo_root/assets/AppIcon.icns"
[[ -f "$icon" ]] || die "app icon not found at $icon (run scripts/make_icon.py)"
cp "$icon" "$contents/Resources/AppIcon.icns"

sed "s/__VERSION__/$version/g" "$repo_root/scripts/Info.plist" > "$contents/Info.plist"

# ── 4. codesign ─────────────────────────────────────────────────────────────
identity="${CODESIGN_IDENTITY:--}"
say "codesigning (identity: $identity)"
codesign --force --deep --sign "$identity" "$app"
codesign --verify --deep --strict "$app" || die "codesign verification failed"

# ── 5. archives ─────────────────────────────────────────────────────────────
if [[ "${SKIP_ARCHIVES:-0}" != "1" ]]; then
    zip_path="$dist/TextLayr-$version.zip"
    dmg_path="$dist/TextLayr-$version.dmg"
    say "archiving $zip_path"
    rm -f "$zip_path"
    ditto -c -k --keepParent "$app" "$zip_path"

    say "building $dmg_path"
    rm -f "$dmg_path"
    "$repo_root/scripts/make_dmg.sh" "$app" "$version" "$dmg_path"
fi

# ── 6. report ───────────────────────────────────────────────────────────────
size="$(du -sh "$app" | cut -f1)"
say "done: $app ($size, version $version)"
echo
echo "Run it:            open \"$app\""
echo "Drag a video onto the window (or, from a terminal, add a path as argv[1])."
echo "First run downloads ffmpeg + a whisper model (~190 MB) into:"
echo "  ~/Library/Application Support/textlayr/"
echo "Prefer to do that headlessly?  \"$contents/MacOS/TextLayr\" --setup"
echo
echo "This build is ad-hoc signed and NOT notarized, so Gatekeeper refuses it on a"
echo "machine that receives it as a browser/AirDrop download (macOS 15 also removed"
echo "the old \"right-click > Open\" bypass). Two ways round it, both in the README:"
echo
echo "  1. have them install with the script, which never sets the quarantine flag:"
echo "       curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh"
echo "  2. or clear the flag on the installed copy:"
echo "       xattr -dr com.apple.quarantine \"/Applications/TextLayr.app\""
echo
echo "For a browser download the user can also allow it once in System Settings >"
echo "Privacy & Security > Open Anyway (admin account, within an hour of trying)."
