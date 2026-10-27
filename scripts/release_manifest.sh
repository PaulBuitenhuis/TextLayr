#!/usr/bin/env bash
#
# Write SHA256SUMS, the app's update manifest and the version-agnostic website
# aliases for a set of packaged artefacts. This is the last step of a release:
# .github/workflows/release.yml calls it after collecting the platform
# artifacts, and it can be run by hand against a downloaded rehearsal build.
#
# Usage:
#   scripts/release_manifest.sh dist 0.4.0
#
# The directory must contain the macOS `TextLayr-<version>.{zip,dmg}`. Linux is
# optional: `TextLayr-<version>-linux-x86_64.tar.xz` is published when present,
# and the AppImage (best-effort, needs `appimagetool` at package time) is added
# when it exists too. That is what keeps a macOS-only release from advertising
# an asset that was never built.
set -euo pipefail

dir="${1:?usage: release_manifest.sh <dir> <version>}"
version="${2:?usage: release_manifest.sh <dir> <version>}"
version="${version#v}"

cd "$dir"

mac_zip="TextLayr-${version}.zip"
mac_dmg="TextLayr-${version}.dmg"
lin_tar="TextLayr-${version}-linux-x86_64.tar.xz"
lin_app="TextLayr-${version}-linux-x86_64.AppImage"

for f in "$mac_zip" "$mac_dmg"; do
    [ -f "$f" ] || { echo "missing release artifact: $f" >&2; exit 1; }
done

# GNU coreutils on the Linux runner; shasum/stat -f for a local macOS dry run.
sha() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
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
    printf '{"name":"%s","sha256":"%s","size":%s}' "$1" "$(sha "$1")" "$(size "$1")"
}

# macOS is mandatory; Linux is included only when the build produced it.
sha_files=("$mac_zip" "$mac_dmg")
platforms="\"macos-arm64\": { \"zip\": $(asset "$mac_zip"), \"dmg\": $(asset "$mac_dmg") }"
if [ -f "$lin_tar" ]; then
    lin_assets="\"tar\": $(asset "$lin_tar")"
    sha_files+=("$lin_tar")
    if [ -f "$lin_app" ]; then
        lin_assets="${lin_assets}, \"appimage\": $(asset "$lin_app")"
        sha_files+=("$lin_app")
    fi
    platforms="${platforms}, \"linux-x86_64\": { ${lin_assets} }"
else
    echo "no Linux artifacts; writing the macOS release only" >&2
fi

sha256sum "${sha_files[@]}" > SHA256SUMS 2>/dev/null ||
    shasum -a 256 "${sha_files[@]}" > SHA256SUMS

# `zip`/`dmg` at the top level stay for backward compatibility; the app reads
# `platforms{}` (keyed by platform::slug()).
cat > latest.json <<EOF
{
  "version": "${version}",
  "notes_url": "https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v${version}",
  "zip": $(asset "$mac_zip"),
  "dmg": $(asset "$mac_dmg"),
  "platforms": { ${platforms} }
}
EOF

python3 -c "import json; json.load(open('latest.json')); print('latest.json is valid JSON')"

# Version-agnostic aliases for the website (after SHA256SUMS, so the checksums
# only name the versioned files).
cp "$mac_dmg" TextLayr.dmg
cp "$mac_zip" TextLayr.zip
if [ -f "$lin_tar" ]; then
    cp "$lin_tar" TextLayr-linux-x86_64.tar.xz
fi
if [ -f "$lin_app" ]; then
    cp "$lin_app" TextLayr-linux-x86_64.AppImage
fi
