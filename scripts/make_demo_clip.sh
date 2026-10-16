#!/usr/bin/env bash
#
# Regenerate the landing-page hero clip from a source video.
#
# Produces (both committed to the repo):
#   docs/assets/demo.mp4          muted, looping, ~8 s, 640 px wide, faststart
#   docs/assets/demo-poster.jpg   still frame shown before the clip plays
#
# Usage:
#   scripts/make_demo_clip.sh <video> [start_seconds] [duration_seconds]
#
# Example:
#   scripts/make_demo_clip.sh "AI job search tips_karaoke.mp4" 17 8
#
# Env:
#   FFMPEG        ffmpeg binary to use (default: ffmpeg on PATH, else the copy
#                 TextLayr downloaded into ~/Library/Application Support/textlayr/bin)
#   WIDTH         output width in px (default 640)
#   CRF           x264 quality, lower = better/bigger (default 30)
#   POSTER_AT     seconds *into the source* for the poster frame
#                 (default: start + duration/3)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

input="${1:-}"
[[ -n "$input" ]] || die "usage: scripts/make_demo_clip.sh <video> [start_seconds] [duration_seconds]"
[[ -f "$input" ]] || die "no such file: $input"

start="${2:-0}"
duration="${3:-8}"
width="${WIDTH:-640}"
crf="${CRF:-30}"

if [[ -z "${POSTER_AT:-}" ]]; then
    poster_at="$(awk -v s="$start" -v d="$duration" 'BEGIN { printf "%.2f", s + d / 3 }')"
else
    poster_at="$POSTER_AT"
fi

if [[ -z "${FFMPEG:-}" ]]; then
    if command -v ffmpeg >/dev/null 2>&1; then
        FFMPEG="$(command -v ffmpeg)"
    elif [[ -x "$HOME/Library/Application Support/textlayr/bin/ffmpeg" ]]; then
        FFMPEG="$HOME/Library/Application Support/textlayr/bin/ffmpeg"
    else
        die "ffmpeg not found: put it on PATH or set FFMPEG=..."
    fi
fi

out_dir="$repo_root/docs/assets"
[[ -d "$out_dir" ]] || die "site assets not found at $out_dir"
mkdir -p "$out_dir"

say "clip: ${duration}s from ${start}s of $input"
"$FFMPEG" -v error -y \
    -ss "$start" -t "$duration" -i "$input" \
    -an \
    -vf "scale=${width}:-2" \
    -c:v libx264 -profile:v high -crf "$crf" -preset slow \
    -pix_fmt yuv420p -movflags +faststart \
    "$out_dir/demo.mp4"

say "poster: frame at ${poster_at}s"
"$FFMPEG" -v error -y \
    -ss "$poster_at" -i "$input" -frames:v 1 \
    -vf "scale=${width}:-2" -q:v 4 \
    "$out_dir/demo-poster.jpg"

say "done"
ls -lh "$out_dir/demo.mp4" "$out_dir/demo-poster.jpg" | awk '{ print "    " $9 "  " $5 }'
printf '\n'
say "next: check docs/index.html (video width/height + poster) and preview with:"
printf '    cd docs && python3 -m http.server 8765\n'
