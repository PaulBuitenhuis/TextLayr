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
#   --cli              also install a `textlayr` command onto your PATH
#   --uninstall        remove the app and everything TextLayr downloaded
#   --yes              don't ask for confirmation (with --uninstall)
#   -h, --help         this text
#
# To pass options through `curl | sh`, use:  ... | sh -s -- --no-open
#
# Uninstall with:  ... | sh -s -- --uninstall
#
set -eu

repo="PaulBuitenhuis/TextLayr"
base="https://github.com/$repo/releases"
app_name="TextLayr.app"
data_dir="$HOME/Library/Application Support/textlayr"
cache_dir="$HOME/Library/Caches/textlayr"
receipt="$data_dir/install.json"

say() { printf '%s\n' "$*"; }
die() { printf 'install: error: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
TextLayr installer for macOS (Apple Silicon, macOS 13 or later).

  curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh

Options
  --dir <path>       install somewhere else (default: /Applications, or
                     ~/Applications when /Applications isn't writable)
  --version <x.y.z>  install a specific release instead of the latest
  --no-open          don't launch TextLayr afterwards
  --cli              also install a `textlayr` command onto your PATH
                     (/usr/local/bin, or ~/.local/bin when that isn't writable)
  --uninstall        remove TextLayr.app plus its downloaded ffmpeg, models,
                     preferences and transcript cache
  --yes              don't ask for confirmation (with --uninstall)
  -h, --help         this text

To pass options through `curl | sh`, use:  ... | sh -s -- --no-open

Uninstall with:  ... | sh -s -- --uninstall
EOF
}

expand_dir() {
    case "$1" in
        "~") printf '%s\n' "$HOME" ;;
        "~/"*) printf '%s\n' "$HOME/${1#\~/}" ;;
        *) printf '%s\n' "$1" ;;
    esac
}

dir=""
version=""
open_after=1
want_cli=0
uninstall=0
assume_yes=0

while [ $# -gt 0 ]; do
    case "$1" in
        --dir) [ $# -ge 2 ] || die "--dir needs a path"; dir="$(expand_dir "$2")"; shift 2 ;;
        --dir=*) dir="$(expand_dir "${1#--dir=}")"; shift ;;
        --version) [ $# -ge 2 ] || die "--version needs something like 0.2.1"; version="$2"; shift 2 ;;
        --version=*) version="${1#--version=}"; shift ;;
        --no-open) open_after=0; shift ;;
        --cli) want_cli=1; shift ;;
        --uninstall) uninstall=1; shift ;;
        --yes|-y) assume_yes=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

# Accept both "0.2.2" and "v0.2.2".
version="${version#v}"

is_running() {
    pgrep -f "$app_name/Contents/MacOS/TextLayr" >/dev/null 2>&1
}

human_size() {
    [ -e "$1" ] || { printf ''; return 0; }
    du -sh "$1" 2>/dev/null | awk '{print $1}'
}

# Read a confirmation from the terminal. `curl | sh` means stdin is the script,
# so /dev/tty is the only safe place to ask.
confirm() {
    [ "$assume_yes" = 1 ] && return 0
    if [ ! -r /dev/tty ]; then
        die "no terminal to confirm on; re-run with --yes"
    fi
    printf '%s [y/N] ' "$1" >&2
    reply=""
    read -r reply </dev/tty || true
    case "$reply" in
        y|Y|yes|YES) return 0 ;;
        *) return 1 ;;
    esac
}

# ── uninstall ──────────────────────────────────────────────────────────────
if [ "$uninstall" = 1 ]; then
    # Where did the app go? An explicit --dir wins, then the receipt written at
    # install time (covers custom paths), then the two standard locations.
    app=""
    if [ -n "$dir" ] && [ -d "$dir/$app_name" ]; then
        app="$dir/$app_name"
    elif [ -f "$receipt" ]; then
        recorded="$(sed -n 's/.*"app"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$receipt" | head -1)"
        # Only trust a receipt that points at an actual TextLayr bundle, so a
        # stale or hand-edited file can never turn into `rm -rf` of something
        # else.
        case "$recorded" in
            */"$app_name") [ -d "$recorded" ] && app="$recorded" ;;
        esac
    fi
    if [ -z "$app" ]; then
        for candidate in "/Applications/$app_name" "$HOME/Applications/$app_name"; do
            if [ -d "$candidate" ]; then app="$candidate"; break; fi
        done
    fi

    if is_running; then
        die "TextLayr is running — quit it and run this again"
    fi

    if [ -z "$app" ] && [ ! -d "$data_dir" ] && [ ! -d "$cache_dir" ]; then
        say "TextLayr doesn't appear to be installed — nothing to remove."
        exit 0
    fi

    say "This will remove:"
    [ -n "$app" ] && say "  $app  ($(human_size "$app"))"
    [ -d "$data_dir" ] && say "  $data_dir  ($(human_size "$data_dir")) — ffmpeg, models, preferences"
    [ -d "$cache_dir" ] && say "  $cache_dir  ($(human_size "$cache_dir")) — transcript cache"
    say ""
    say "Videos you exported are not touched."

    confirm "Remove TextLayr?" || { say "Cancelled."; exit 0; }

    # Remove our `textlayr` launcher, but only if it points into a TextLayr
    # bundle (a symlink, or the wrapper written by --cli).
    for link in "/usr/local/bin/textlayr" "$HOME/.local/bin/textlayr"; do
        if [ -L "$link" ]; then
            case "$(readlink "$link")" in
                *"$app_name/Contents/MacOS/TextLayr") rm -f "$link" ;;
            esac
        elif [ -f "$link" ] && grep -q "exec \"[^\"]*$app_name/Contents/MacOS/TextLayr\"" "$link" 2>/dev/null; then
            rm -f "$link"
        fi
    done

    [ -n "$app" ] && rm -rf "$app"
    [ -d "$data_dir" ] && rm -rf "$data_dir"
    [ -d "$cache_dir" ] && rm -rf "$cache_dir"

    say "==> TextLayr is uninstalled."
    exit 0
fi

# ── 1. the release manifest ────────────────────────────────────────────────
# Platform checks live here, after `--uninstall`: removing an old install must
# work even on a Mac the current build no longer runs on.
[ "$(uname -s)" = "Darwin" ] || die "this installer is for macOS"

# `uname -m` reports x86_64 in a Terminal running under Rosetta, so ask the
# kernel whether this is actually Apple Silicon.
if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || printf '0')" != "1" ]; then
    die "TextLayr requires an Apple Silicon Mac; there is no Intel build (this Mac reports $(uname -m))"
fi

os_version="$(sw_vers -productVersion)"
os_major="${os_version%%.*}"
[ "$os_major" -ge 13 ] 2>/dev/null \
    || die "TextLayr requires macOS 13 or later (this Mac is on macOS $os_version)"

# This is the same file the app's own updater reads, so the name and checksum
# below are the ones TextLayr itself would trust. No GitHub API, no rate limit.
url="$base/latest/download/latest.json"
[ -n "$version" ] && url="$base/download/v$version/latest.json"

say "==> looking up the newest release"
json="$(curl -fsSL --proto '=https' --tlsv1.2 "$url" </dev/null)" \
    || die "could not fetch $url — check your connection, or the version you asked for"

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
    -o "$work/$zip_name" "$base/download/v$tag/$zip_name" </dev/null \
    || die "download failed — nothing was installed (check your connection and try again)"

got="$(shasum -a 256 "$work/$zip_name" | awk '{print $1}')"
[ "$got" = "$zip_sha" ] \
    || die "checksum mismatch — expected $zip_sha but got $got (download corrupted? nothing was installed)"

# ── 3. unpack ──────────────────────────────────────────────────────────────
# `ditto` rather than `unzip`: it keeps the signature and extended attributes.
mkdir -p "$work/out"
ditto -x -k "$work/$zip_name" "$work/out"
src="$work/out/$app_name"
[ -d "$src" ] || die "the archive didn't contain $app_name"

# ── 4. choose where it goes ────────────────────────────────────────────────
if [ -z "$dir" ]; then
    if [ -w /Applications ]; then
        dir="/Applications"
    else
        dir="$HOME/Applications"
    fi
fi
mkdir -p "$dir"
dest="$dir/$app_name"

if [ -d "$dest" ] && is_running; then
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

# Remember where it went so `--uninstall` can find a custom --dir later.
mkdir -p "$data_dir"
printf '{\n  "app": "%s",\n  "version": "%s"\n}\n' "$dest" "$tag" > "$receipt"

# ── 6. optional command-line link ──────────────────────────────────────────
if [ "$want_cli" = 1 ]; then
    if [ -w /usr/local/bin ]; then
        bindir="/usr/local/bin"
    else
        bindir="$HOME/.local/bin"
    fi
    mkdir -p "$bindir"
    # A two-line launcher rather than a symlink: it invokes the binary by its
    # real bundle path, so resource lookup works even for releases that predate
    # the current_exe canonicalization fix. With no arguments, launch the bundle
    # properly instead of running the bare executable through Terminal.
    cat > "$bindir/textlayr" <<EOF
#!/bin/sh
if [ \$# -eq 0 ]; then
    exec open "$dest"
fi
exec "$dest/Contents/MacOS/TextLayr" "\$@"
EOF
    chmod +x "$bindir/textlayr"
    say "==> installed $bindir/textlayr"
    case ":$PATH:" in
        *":$bindir:"*) ;;
        *)
            say ""
            say "Add $bindir to your PATH to use it:"
            say "  export PATH=\"$bindir:\$PATH\""
            ;;
    esac
fi

say "==> done — TextLayr $tag is installed"
say ""
say "The first launch needs an internet connection and downloads ffmpeg and a"
say "whisper model — about 190 MB for the default base.en model, or up to about"
say "530 MB for small.en. After that it works offline."
say ""

if [ "$open_after" = 1 ]; then
    open "$dest"
else
    say "Open it from Launchpad, Spotlight, or: open \"$dest\""
fi
