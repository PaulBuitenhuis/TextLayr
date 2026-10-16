#!/usr/bin/env bash
#
# Serve a fake update release locally so the in-app updater can be exercised
# without publishing anything to GitHub.
#
# It packages TextLayr at a bogus-but-higher version (so the running build sees
# an "update"), writes the same `latest.json` the release workflow produces,
# and serves the folder with `python3 -m http.server`. Point a **scratch copy**
# of the app at it with $TEXTLAYR_UPDATE_URL and click "Download & install" in
# About → Updates.
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

zip_name="$(basename "$zip")"
dmg_name="$(basename "$dmg")"
zip_sha="$(shasum -a 256 "$zip" | awk '{print $1}')"
dmg_sha="$(shasum -a 256 "$dmg" | awk '{print $1}')"
zip_size="$(stat -f%z "$zip")"
dmg_size="$(stat -f%z "$dmg")"

say "writing dist/latest.json"
cat > dist/latest.json <<EOF
{
  "version": "$version",
  "notes_url": "https://github.com/PaulBuitenhuis/TextLayr/releases",
  "zip": { "name": "$zip_name", "sha256": "$zip_sha", "size": $zip_size },
  "dmg": { "name": "$dmg_name", "sha256": "$dmg_sha", "size": $dmg_size }
}
EOF

echo
say "now run the scratch copy against this server:"
cat <<EOF

  mkdir -p ~/Applications/TextLayr-test
  rm -rf ~/Applications/TextLayr-test/TextLayr.app
  cp -R dist/TextLayr.app ~/Applications/TextLayr-test/
  TEXTLAYR_UPDATE_URL="http://127.0.0.1:$port/latest.json" \\
    ~/Applications/TextLayr-test/TextLayr.app/Contents/MacOS/TextLayr

Then: About -> Updates -> "Download & install".

EOF

say "serving dist/ on http://127.0.0.1:$port  (Ctrl-C to stop)"
cd dist
exec python3 -m http.server "$port" --bind 127.0.0.1
