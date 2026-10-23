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
# The served app is version `<version>`; the running binary reports the version
# compiled from Cargo.toml (e.g. 0.2.0), so 9.9.9 (the default) looks newer and
# the extracted bundle still validates (its Info.plist matches the manifest).
#
# Usage:
#   scripts/serve_updates_local.sh [version] [port]
#
# Then, in another terminal (replace the port if you changed it):
#   mkdir -p ~/Applications/TextLayr-test
#   rm -rf ~/Applications/TextLayr-test/TextLayr.app
#   cp -R dist/TextLayr.app ~/Applications/TextLayr-test/
#   TEXTLAYR_UPDATE_URL="http://127.0.0.1:8000/latest.json" \
#     ~/Applications/TextLayr-test/TextLayr.app/Contents/MacOS/TextLayr
#
# The updater will swap ~/Applications/TextLayr-test/TextLayr.app and relaunch
# it — your real install is never touched. Ctrl-C here stops the server.
set -euo pipefail

version="${1:-9.9.9}"
port="${2:-8000}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

say "packaging TextLayr $version (into dist/)"
TEXTLAYR_VERSION="$version" scripts/package_macos.sh

shopt -s nullglob
zip="dist/TextLayr-$version.zip"
dmg="dist/TextLayr-$version.dmg"
shopt -u nullglob

[[ -f "$zip" ]] || { echo "error: expected $zip" >&2; exit 1; }
[[ -f "$dmg" ]] || { echo "error: expected $dmg" >&2; exit 1; }

sha() { shasum -a 256 "$1" | awk '{print $1}'; }
size() { stat -f%z "$1"; }
asset() {
    printf '{"name":"%s","sha256":"%s","size":%s}' \
        "$(basename "$1")" "$(sha "$1")" "$(size "$1")"
}

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
python3 -c "import json; json.load(open('dist/latest.json')); print('latest.json is valid JSON')"

echo
say "now run the scratch copy against this server:"
cat <<EOF

  mkdir -p ~/Applications/TextLayr-test
  rm -rf ~/Applications/TextLayr-test/TextLayr.app
  cp -R dist/TextLayr.app ~/Applications/TextLayr-test/
  TEXTLAYR_UPDATE_URL="http://127.0.0.1:$port/latest.json" \\
    ~/Applications/TextLayr-test/TextLayr.app/Contents/MacOS/TextLayr

Then: Settings -> Updates -> "Download & install".

EOF

say "serving dist/ on http://127.0.0.1:$port  (Ctrl-C to stop)"
cd dist
exec python3 -m http.server "$port" --bind 127.0.0.1
