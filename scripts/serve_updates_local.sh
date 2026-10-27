#!/usr/bin/env bash
#
# Serve a fake update release locally so the in-app updater can be exercised
# without publishing anything to GitHub.
#
# It packages TextLayr at a bogus-but-higher version (so the running build sees
# an "update"), writes the same `latest.json` the release workflow produces,
# and serves the folder with `python3 -m http.server`. Point a **scratch copy**
# of the app at it with $TEXTLAYR_UPDATE_URL and click "Download & install" in
# Settings → Updates.
#
# Usage:
#   scripts/serve_updates_local.sh [version] [port]
#
# Env:
#   TEXTLAYR_TEST_PACKAGE  Linux only: use this .tar.xz instead of building one.
#                          Handy when the build machine has no Rust toolchain —
#                          point it at a downloaded CI artefact.
#
# macOS: the served app is version `<version>`; the running binary reports the
# version compiled from Cargo.toml, so 9.9.9 (the default) looks newer and the
# extracted bundle still validates (its Info.plist matches the manifest). The
# updater swaps the scratch copy and relaunches it — your real install is never
# touched. Then, in another terminal:
#
#   mkdir -p ~/Applications/TextLayr-test
#   rm -rf ~/Applications/TextLayr-test/TextLayr.app
#   cp -R dist/TextLayr.app ~/Applications/TextLayr-test/
#   TEXTLAYR_UPDATE_URL="http://127.0.0.1:8000/latest.json" \
#     ~/Applications/TextLayr-test/TextLayr.app/Contents/MacOS/TextLayr
#
# Linux: there is no in-place update yet; the updater downloads the package to
# ~/Downloads and reveals it, so any running TextLayr (scratch or real) is safe.
# The script prints the exact command for the copy you want to test.
#
# Ctrl-C stops the server.
set -euo pipefail

version="${1:-9.9.9}"
port="${2:-8000}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

sha() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}
size() {
    if [[ "$(uname -s)" == "Darwin" ]]; then
        stat -f%z "$1"
    else
        stat -c%s "$1"
    fi
}
asset() {
    printf '{"name":"%s","sha256":"%s","size":%s}' \
        "$(basename "$1")" "$(sha "$1")" "$(size "$1")"
}

case "$(uname -s)" in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    *) die "this script is for macOS and Linux (this is $(uname -s))" ;;
esac

if [[ "$os" == "macos" ]]; then
    say "packaging TextLayr $version (into dist/)"
    TEXTLAYR_VERSION="$version" scripts/package_macos.sh

    shopt -s nullglob
    zip="dist/TextLayr-$version.zip"
    dmg="dist/TextLayr-$version.dmg"
    shopt -u nullglob

    [[ -f "$zip" ]] || die "expected $zip"
    [[ -f "$dmg" ]] || die "expected $dmg"

    say "writing dist/latest.json"
    # Same shape as .github/workflows/release.yml: legacy top-level macOS assets
    # plus the per-platform map the app actually reads.
    cat > dist/latest.json <<EOF
{
  "version": "$version",
  "notes_url": "https://github.com/PaulBuitenhuis/TextLayr/releases",
  "zip": $(asset "$zip"),
  "dmg": $(asset "$dmg"),
  "platforms": {
    "macos-arm64": { "zip": $(asset "$zip"), "dmg": $(asset "$dmg") }
  }
}
EOF
else
    if [[ -n "${TEXTLAYR_TEST_PACKAGE:-}" ]]; then
        [[ -f "$TEXTLAYR_TEST_PACKAGE" ]] || die "no such package: $TEXTLAYR_TEST_PACKAGE"
        tarball="dist/$(basename "$TEXTLAYR_TEST_PACKAGE")"
        cp -f "$TEXTLAYR_TEST_PACKAGE" "$tarball"
        appimage=""
        say "reusing $TEXTLAYR_TEST_PACKAGE (no rebuild)"
    else
        tarball="dist/TextLayr-$version-linux-x86_64.tar.xz"
        appimage="dist/TextLayr-$version-linux-x86_64.AppImage"
        if [[ ! -f "$tarball" ]]; then
            say "packaging TextLayr $version (into dist/)"
            TEXTLAYR_VERSION="$version" scripts/package_linux.sh
        else
            say "reusing existing $tarball"
        fi
        [[ -f "$appimage" ]] || appimage=""
    fi
    [[ -f "$tarball" ]] || die "expected $tarball"

    # Linux never self-replaces, so the manifest only needs the platform map;
    # the legacy top-level zip/dmg entries are macOS-only.
    if [[ -n "$appimage" ]]; then
        platforms="\"linux-x86_64\": { \"tar\": $(asset "$tarball"), \"appimage\": $(asset "$appimage") }"
    else
        platforms="\"linux-x86_64\": { \"tar\": $(asset "$tarball") }"
    fi

    say "writing dist/latest.json"
    cat > dist/latest.json <<EOF
{
  "version": "$version",
  "notes_url": "https://github.com/PaulBuitenhuis/TextLayr/releases",
  "platforms": { ${platforms} }
}
EOF
fi

python3 -c "import json; json.load(open('dist/latest.json')); print('latest.json is valid JSON')"

echo
say "now run the copy you want to test against this server:"
if [[ "$os" == "macos" ]]; then
    cat <<EOF

  mkdir -p ~/Applications/TextLayr-test
  rm -rf ~/Applications/TextLayr-test/TextLayr.app
  cp -R dist/TextLayr.app ~/Applications/TextLayr-test/
  TEXTLAYR_UPDATE_URL="http://127.0.0.1:$port/latest.json" \\
    ~/Applications/TextLayr-test/TextLayr.app/Contents/MacOS/TextLayr

Then: Settings -> Updates -> "Download & install" (swaps the scratch copy).

EOF
else
    cat <<EOF

  TEXTLAYR_UPDATE_URL="http://127.0.0.1:$port/latest.json" \\
    ./TextLayr-<whatever-you-installed>/textlayr

Then: Settings -> Updates -> "Download & install". Linux has no in-place
update yet: the package is downloaded to ~/Downloads and shown in your file
manager, so the running copy is untouched.

EOF
fi

say "serving dist/ on http://127.0.0.1:$port  (Ctrl-C to stop)"
cd dist
exec python3 -m http.server "$port" --bind 127.0.0.1
