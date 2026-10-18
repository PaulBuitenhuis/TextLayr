#!/bin/sh
#
# TextLayr installer for macOS.
#
#   curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh
#
# Why this exists: macOS Gatekeeper refuses to open an app that carries the
# `com.apple.quarantine` attribute — the flag Safari, Chrome and AirDrop add to
# anything you download. Files fetched with `curl` never get that flag, so
# installing this way skips the whole "unidentified developer" dance: no
# password, no trip to System Settings.
#
# What it does
#   1. reads the release manifest the app's own updater trusts
#   2. downloads the .zip over HTTPS and verifies its SHA-256
#   3. unpacks it with `ditto`, which preserves the code signature
#   4. installs to /Applications, or ~/Applications when that isn't writable
#   5. clears any quarantine flag (insurance) and launches it
#
# Options
#   --dir <path>       install somewhere else
#   --version <x.y.z>  install a specific release instead of the latest
#   --no-open          don't launch TextLayr afterwards
#   -h, --help         this text
#
# To pass options through `curl | sh`, use:  ... | sh -s -- --no-open
#
# Uninstall with:  rm -rf /Applications/TextLayr.app
#
set -eu

repo="PaulBuitenhuis/TextLayr"
base="https://github.com/$repo/releases"

say() { printf '%s\n' "$*"; }
die() { printf 'install: error: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
TextLayr installer for macOS.

  curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh

Options
  --dir <path>       install somewhere else (default: /Applications, or
                     ~/Applications when /Applications isn't writable)
  --version <x.y.z>  install a specific release instead of the latest
  --no-open          don't launch TextLayr afterwards
  -h, --help         this text

To pass options through `curl | sh`, use:  ... | sh -s -- --no-open

Uninstall with:  rm -rf /Applications/TextLayr.app
EOF
}

dir=""
version=""
open_after=1

while [ $# -gt 0 ]; do
    case "$1" in
        --dir) [ $# -ge 2 ] || die "--dir needs a path"; dir="$2"; shift 2 ;;
        --dir=*) dir="${1#--dir=}"; shift ;;
        --version) [ $# -ge 2 ] || die "--version needs something like 0.2.1"; version="$2"; shift 2 ;;
        --version=*) version="${1#--version=}"; shift ;;
        --no-open) open_after=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

[ "$(uname -s)" = "Darwin" ] || die "this installer is for macOS"
[ "$(uname -m)" = "arm64" ] || die "TextLayr builds are Apple Silicon only (this Mac is $(uname -m)); see CONTRIBUTING.md to build from source"

# ── 1. the release manifest ────────────────────────────────────────────────
# This is the same file the app's own updater reads, so the name and checksum
# below are the ones TextLayr itself would trust. No GitHub API, no rate limit.
url="$base/latest/download/latest.json"
[ -n "$version" ] && url="$base/download/v$version/latest.json"

say "==> looking up the newest release"
json="$(curl -fsSL --proto '=https' --tlsv1.2 "$url" </dev/null)" \
    || die "could not fetch $url (offline?)"

zip_name="$(printf '%s\n' "$json" | grep '"zip"' | head -1 | sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
zip_sha="$(printf '%s\n' "$json" | grep '"zip"' | head -1 | sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
tag="$(printf '%s\n' "$json" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$zip_name" ] && [ -n "$zip_sha" ] && [ -n "$tag" ] \
    || die "could not parse the release manifest"

# ── 2. download + verify ───────────────────────────────────────────────────
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

say "==> downloading TextLayr $tag"
curl -fL --proto '=https' --tlsv1.2 --progress-bar \
    -o "$work/$zip_name" "$base/download/v$tag/$zip_name" </dev/null

got="$(shasum -a 256 "$work/$zip_name" | awk '{print $1}')"
[ "$got" = "$zip_sha" ] \
    || die "checksum mismatch — expected $zip_sha but got $got (download corrupted? nothing was installed)"

# ── 3. unpack ──────────────────────────────────────────────────────────────
# `ditto` rather than `unzip`: it keeps the signature and extended attributes.
mkdir -p "$work/out"
ditto -x -k "$work/$zip_name" "$work/out"
src="$work/out/TextLayr.app"
[ -d "$src" ] || die "the archive didn't contain TextLayr.app"

# ── 4. choose where it goes ────────────────────────────────────────────────
if [ -z "$dir" ]; then
    if [ -w /Applications ]; then
        dir="/Applications"
    else
        dir="$HOME/Applications"
    fi
fi
mkdir -p "$dir"
dest="$dir/TextLayr.app"

if [ -d "$dest" ] && pgrep -f "TextLayr.app/Contents/MacOS/TextLayr" >/dev/null 2>&1; then
    die "TextLayr is running — quit it and run this again"
fi

# ── 5. install ─────────────────────────────────────────────────────────────
say "==> installing to $dest"
rm -rf "$dest.new"
ditto "$src" "$dest.new"
# The download shouldn't be quarantined at all; this is belt and braces (and
# fixes an existing copy that was installed the browser way).
xattr -cr "$dest.new" 2>/dev/null || true
rm -rf "$dest"
mv "$dest.new" "$dest"

# If something did damage the signature in transit, re-sign it locally — a
# locally built signature carries no quarantine problem.
if command -v codesign >/dev/null 2>&1; then
    if ! codesign --verify --deep --strict "$dest" >/dev/null 2>&1; then
        say "==> re-signing (the bundled signature didn't survive)"
        codesign --force --deep --sign - "$dest" >/dev/null 2>&1 || true
    fi
fi

say "==> done — TextLayr $tag is installed"
say ""
say "The first launch downloads ffmpeg and a whisper model (~190 MB) into"
say "~/Library/Application Support/textlayr/. After that it works offline."
say ""

if [ "$open_after" = 1 ]; then
    open "$dest"
else
    say "Open it from Launchpad, Spotlight, or: open \"$dest\""
fi
