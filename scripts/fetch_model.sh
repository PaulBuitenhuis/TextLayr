#!/usr/bin/env bash
#
# Fetch the whisper small.en model (~465 MB) into models/.
#
# Already-present files are left alone. Override the destination with the
# first argument or $TEXTLAYR_MODEL.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:-${TEXTLAYR_MODEL:-$repo_root/models/ggml-small.en.bin}}"
url="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"

if [[ -f "$dest" ]]; then
    echo "model already present: $dest"
    exit 0
fi

mkdir -p "$(dirname "$dest")"
echo "downloading whisper small.en (~465 MB) -> $dest"
curl -L --fail --progress-bar -o "$dest.part" "$url"
mv "$dest.part" "$dest"
echo "done: $dest"
