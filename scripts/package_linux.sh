#!/usr/bin/env bash
#
# Build Linux release artifacts for TextLayr (x86_64):
#   dist/TextLayr-<version>-linux-x86_64.tar.xz    portable tarball
#   dist/TextLayr-<version>-linux-x86_64.AppImage  single-file portable app
#
# Both contain the binary and the caption fonts; ffmpeg/ffprobe and the whisper
# model are downloaded on first run.
#
# Usage:
#   scripts/package_linux.sh
#
# Env:
#   TEXTLAYR_VERSION   version string (default: `git describe`, else Cargo.toml)
#   SKIP_APPIMAGE=1    build only the tarball (no appimagetool required)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ── 1. release build ────────────────────────────────────────────────────────
say "building release binary"
cargo build --release
bin="$repo_root/target/release/textlayr"
[[ -x "$bin" ]] || die "release binary not found at $bin"

# ── 2. version ──────────────────────────────────────────────────────────────
version="${TEXTLAYR_VERSION:-}"
if [[ -z "$version" ]]; then
    version="$(git describe --tags --always 2>/dev/null || true)"
fi
if [[ -z "$version" ]]; then
    version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
fi
[[ -n "$version" ]] || die "could not determine a version (set TEXTLAYR_VERSION)"
version="${version#v}"

name="TextLayr-$version-linux-x86_64"
stage="$repo_root/dist/$name"

# ── 3. portable tarball ─────────────────────────────────────────────────────
say "assembling $stage"
rm -rf "$stage"
mkdir -p "$stage/assets/fonts"
cp "$bin" "$stage/textlayr"
cp "$repo_root/assets/fonts"/*.ttf "$stage/assets/fonts/"
[[ -f "$repo_root/assets/fonts/OFL.txt" ]] && cp "$repo_root/assets/fonts/OFL.txt" "$stage/assets/fonts/"
cp "$repo_root/LICENSE" "$stage/LICENSE"
cat > "$stage/README.txt" <<EOF
TextLayr $version — Linux (x86_64)

  ./textlayr            the app (run it, or double-click in a file manager)

First run downloads ffmpeg + a whisper model:

  ./textlayr --setup

Then drag a video onto the window, or run:

  ./textlayr video.mp4
EOF

tarball="$repo_root/dist/$name.tar.xz"
say "archiving $tarball"
rm -f "$tarball"
tar -C "$repo_root/dist" -cJf "$tarball" "$name"

# ── 4. AppImage ─────────────────────────────────────────────────────────────
if [[ "${SKIP_APPIMAGE:-0}" == "1" ]]; then
    say "SKIP_APPIMAGE=1 — skipping the AppImage"
else
    appimagetool="$(command -v appimagetool || true)"
    if [[ -z "$appimagetool" ]]; then
        say "appimagetool not found — skipping the AppImage (set SKIP_APPIMAGE=1 to silence)"
    else
        appdir="$repo_root/dist/TextLayr.AppDir"
        say "building $appdir"
        rm -rf "$appdir"
        mkdir -p "$appdir/usr/bin" \
                 "$appdir/usr/share/textlayr/assets/fonts" \
                 "$appdir/usr/share/icons/hicolor" \
                 "$appdir/usr/share/applications"
        cp "$bin" "$appdir/usr/bin/textlayr"
        cp "$repo_root/assets/fonts"/*.ttf "$appdir/usr/share/textlayr/assets/fonts/"
        [[ -f "$repo_root/assets/fonts/OFL.txt" ]] && \
            cp "$repo_root/assets/fonts/OFL.txt" "$appdir/usr/share/textlayr/assets/fonts/"
        cp -R "$repo_root/assets/icons/hicolor/." "$appdir/usr/share/icons/hicolor/"
        cp "$repo_root/assets/icons/hicolor/256x256/apps/textlayr.png" "$appdir/textlayr.png"
        cp "$appdir/textlayr.png" "$appdir/.DirIcon"
        cat > "$appdir/textlayr.desktop" <<'EOF'
[Desktop Entry]
Name=TextLayr
Comment=Offline karaoke captions for short-form video
Exec=textlayr %F
Icon=textlayr
MimeType=video/*;
Categories=AudioVideo;Video;
Terminal=false
Type=Application
EOF
        cp "$appdir/textlayr.desktop" "$appdir/usr/share/applications/"
        ln -sf usr/bin/textlayr "$appdir/AppRun"

        appimage="$repo_root/dist/$name.AppImage"
        say "building $appimage"
        rm -f "$appimage"
        # `--appimage-extract-and-run` lets the tool run without FUSE (CI/containers).
        ARCH=x86_64 "$appimagetool" --appimage-extract-and-run "$appdir" "$appimage" \
            || ARCH=x86_64 "$appimagetool" "$appdir" "$appimage"
    fi
fi

# ── 5. report ───────────────────────────────────────────────────────────────
say "done — artifacts in $repo_root/dist/"
ls -1 "$repo_root/dist" | grep -E "TextLayr-${version}-linux-x86_64" || true
