#!/usr/bin/env bash
#
# Build a self-contained texxy.app for macOS (Apple Silicon / Intel).
#
# The bundle is fully offline: the release binary plus ffmpeg, ffprobe, the
# whisper model and the caption fonts all live under Contents/Resources, which
# is exactly where src/probe.rs, src/model.rs and src/config.rs look first.
#
# Usage:
#   scripts/package_macos.sh
#
# Inputs (each can be overridden by an environment variable):
#   TEXXY_FFMPEG_DIR  directory holding the ffmpeg + ffprobe to bundle
#                     (default: vendor/macos/, then $PATH)
#   TEXXY_MODEL       path to ggml-small.en.bin
#                     (default: models/ggml-small.en.bin)
#   TEXXY_VERSION     CFBundle version string (default: `git describe` or 0.1.0)
#   CODESIGN_IDENTITY codesign identity; "-" = ad-hoc (default). Set to a
#                     Developer ID to ship outside this machine.
#
# Output: dist/texxy.app
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

app="$repo_root/dist/texxy.app"
contents="$app/Contents"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ── 1. release build ────────────────────────────────────────────────────────
say "building release binary"
cargo build --release

binary="$repo_root/target/release/texxy"
[[ -x "$binary" ]] || die "release binary not found at $binary"

# ── 2. locate the pieces to bundle ──────────────────────────────────────────
find_tool() {  # find_tool <name>
    local name="$1"
    if [[ -n "${TEXXY_FFMPEG_DIR:-}" ]]; then
        [[ -f "$TEXXY_FFMPEG_DIR/$name" ]] || die "$TEXXY_FFMPEG_DIR/$name not found"
        echo "$TEXXY_FFMPEG_DIR/$name"
    elif [[ -f "$repo_root/vendor/macos/$name" ]]; then
        echo "$repo_root/vendor/macos/$name"
    elif command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
    else
        die "$name not found. Put a static build in vendor/macos/ (or set TEXXY_FFMPEG_DIR), or brew install ffmpeg."
    fi
}
ffmpeg_src="$(find_tool ffmpeg)"
ffprobe_src="$(find_tool ffprobe)"

model_src="${TEXXY_MODEL:-$repo_root/models/ggml-small.en.bin}"
[[ -f "$model_src" ]] || die "whisper model not found at $model_src — run scripts/fetch_model.sh or set TEXXY_MODEL"

fonts_src="$repo_root/assets/fonts"
[[ -d "$fonts_src" ]] || die "bundled fonts not found at $fonts_src"

version="${TEXXY_VERSION:-}"
if [[ -z "$version" ]]; then
    version="$(git describe --tags --always 2>/dev/null || echo 0.1.0)"
fi

# ── 3. assemble the bundle ──────────────────────────────────────────────────
say "assembling $app"
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources/bin" "$contents/Resources/models" "$contents/Resources/assets/fonts"

cp "$binary" "$contents/MacOS/texxy"
chmod +x "$contents/MacOS/texxy"

cp "$ffmpeg_src"  "$contents/Resources/bin/ffmpeg"
cp "$ffprobe_src" "$contents/Resources/bin/ffprobe"
chmod +x "$contents/Resources/bin/ffmpeg" "$contents/Resources/bin/ffprobe"

cp "$model_src" "$contents/Resources/models/ggml-small.en.bin"
cp "$fonts_src"/*.ttf "$contents/Resources/assets/fonts/"

sed "s/__VERSION__/$version/g" "$repo_root/scripts/Info.plist" > "$contents/Info.plist"

# ── 4. codesign ─────────────────────────────────────────────────────────────
identity="${CODESIGN_IDENTITY:--}"
say "codesigning (identity: $identity)"
codesign --force --sign "$identity" "$contents/Resources/bin/ffmpeg"
codesign --force --sign "$identity" "$contents/Resources/bin/ffprobe"
codesign --force --deep --sign "$identity" "$app"
codesign --verify --deep --strict "$app" || die "codesign verification failed"

# ── 5. report ───────────────────────────────────────────────────────────────
size="$(du -sh "$app" | cut -f1)"
say "done: $app ($size, version $version)"
echo
echo "Run it:            open \"$app\""
echo "Drag a video onto the window (or, from a terminal, add a path as argv[1])."
echo "First launch of an ad-hoc-signed app may need Right click > Open, or:"
echo "  xattr -dr com.apple.quarantine \"$app\""
