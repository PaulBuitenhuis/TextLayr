# texxy

Offline karaoke caption burner for short-form video. Drop in a video, review
the transcript, export a new video with word-synced popup captions baked in.

**Design & plan:** see [DESIGN.md](./DESIGN.md).

**Status:** Milestone 2 (caption renderer + CLI export). The full pipeline runs
headlessly; the GUI comes in Milestone 3.

## Requirements

| Tool | Why | Notes |
|---|---|---|
| Rust (stable) | build | via `rustup` |
| `clang` | builds whisper.cpp | present on Omarchy |
| `make`, `g++` | C/C++ build | present |
| `cmake` | whisper-rs builds whisper.cpp | see setup below if not on PATH |
| `ffmpeg` + `ffprobe` | probe, audio extract, compositing, encode | system install is fine in dev |

## One-time setup

1. **Toolchain** (if missing):
   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   ```
2. **cmake** — if not installed system-wide and you lack sudo, drop a standalone
   build somewhere on your `PATH` (e.g. `~/.local/bin`), including its `share/`
   directory (e.g. `~/.local/share/cmake-<version>`), or install via your distro.
3. **Model** — download once into `models/`:
   ```sh
   cd models
   curl -L -o ggml-small.en.bin \
     "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
   ```
   (~465 MB, English-optimized, offline thereafter. Overridable with `TEXXY_MODEL`.)
4. **Fonts** — already bundled in `assets/fonts/` (OFL-licensed, ~1 MB):
   Inter, Nunito, Libre Baskerville, each in Regular + Bold. To re-fetch:
   ```sh
   cd assets/fonts
   for fam in "Inter:Inter" "Nunito:Nunito" "Libre+Baskerville:LibreBaskerville"; do
     q="${fam%:*}"; n="${fam##*:}"; i=0
     for u in $(curl -s "https://fonts.googleapis.com/css2?family=$q" | grep -oE 'https://[^)]+\.ttf'); do
       [ $i -eq 0 ] && w=Regular || w=Bold
       curl -sSL -o "$n-$w.ttf" "$u"; i=$((i+1))
     done
   done
   ```
   (Google Fonts' CSS API returns static TrueType when called without a
   browser User-Agent.)

## Build & run

```sh
cargo build
cargo run -- "../samples/AI job search tips.mp4"      # full pipeline -> *_karaoke.mp4
```

Pipeline: `ffprobe` (display dims + rotation) → ffmpeg audio extract (16 kHz mono
f32le) → whisper transcription with DTW word timings (cached) → chunking →
caption state PNGs → ffmpeg overlay graph → `AI job search tips_karaoke.mp4`
(H.264 crf 18, audio copied).

```sh
texxy <video> [--words] [--no-cache] [-o out.mp4] [--png-dir dir]
        [--font <family>] [--font-size <pct>] [--position <pct>] [--words-per <n>]
```

| Option | Effect |
|---|---|
| `--words` | Milestone 1 behavior: print the `(start, end, word)` table and stop |
| `--no-cache` | Ignore the cached transcript and re-transcribe |
| `-o, --output` | Output path (default: `<input>_karaoke.mp4` next to the input) |
| `--png-dir` | Render caption states + `filter_complex.txt` + `states.txt`, skip encoding |
| `--font` | `Libre Baskerville` (default) · `Inter` · `Nunito` |
| `--font-size` | Glyph size as % of frame height (default 3.4) |
| `--position` | Caption box center as % of frame height (default 72) |
| `--words-per` | Words per popup, 2–6 (default 3) |

Suppress progress on stderr, keep the table on stdout:

```sh
./target/debug/texxy video.mp4 --words 2>/dev/null
```

## Development notes

- **Vendored `whisper-rs-sys` patch.** Upstream `whisper-rs-sys` regenerates
  bindgen bindings at build time; on this glibc that emits a `_IO_FILE` layout
  assertion that fails to compile, and an opaque `whisper_full_params`. We vendor
  a copy under `vendor/patch/whisper-rs-sys` (wired via `[patch.crates-io]`) that
  uses the crate's prebuilt bindings and strips their compile-time layout tests.
  Remove the patch once upstream ships a fix.
- **DTW word timings.** Plain whisper token timestamps collapse several words onto
  one boundary. We enable DTW (`DtwMode::ModelPreset::SmallEn`) on the context for
  accurate per-word timings, plus a normalization pass that spreads any remaining
  identical-timestamp clusters evenly and enforces a minimum word duration.
- **whisper C logging** is redirected away from stdout via
  `whisper_rs::install_logging_hooks()` (no log backend → logs dropped).
- **Transcript cache.** whisper small.en costs ~6 min on the 222 s sample here, so
  word timings are cached as JSON in the platform cache dir
  (`~/.cache/texxy/<hash>.json`), keyed on input path + size + mtime. Every render
  or style iteration then starts instantly. `--no-cache` forces a re-transcription.
- **Caption states, not subtitles.** Each chunk of N words is rendered once per
  highlighted word ("state") — a small RGBA PNG of just the caption box, with a
  transparent surround. ffmpeg composites each state only during its word's
  `enable='gte(t,a)*lt(t,b)'` window. Tight crops keep ~750 states at ~300 KB
  instead of ~8 MB each (a full-frame approach would need ~4.5 GB of RSS).
- **Still-image inputs, never `-loop`.** A looping image input would re-decode the
  PNG for every frame; a plain `-i state.png` decodes once and the overlay repeats
  it (`eof_action=repeat`).
- **Minimum highlight windows.** Whisper's word starts can land 20 ms apart;
  `Chunk::state_windows()` forces windows to be monotonic with a ≥0.1 s step that
  tiles the chunk's span, so a highlight can't flash for two frames.
- **ffmpeg 8/9 dropped `-filter_complex_script`.** It was replaced by the generic
  "option value from file" syntax `-/filter_complex <file>`. `export.rs` probes
  `ffmpeg -h full` once and uses whichever spelling the installed build accepts.

## Layout

```
src/
  main.rs        CLI pipeline: probe → transcribe → chunk → render → export
  probe.rs       ffprobe wrapper (rotation-aware display dims) + ffmpeg discovery
  audio.rs       16 kHz mono f32le extraction + PCM read
  transcribe.rs  whisper transcription, word merge, timing normalization, cache
  model.rs       whisper model discovery
  chunk.rs       word grouping into caption chunks + highlight windows
  config.rs      Style struct (fonts, colors, sizes) + font discovery
  render.rs      fontdue + tiny-skia -> per-state caption PNGs
  export.rs      ffmpeg overlay filter graph construction + encode
assets/fonts/    bundled caption fonts (Inter, Nunito, Libre Baskerville, OFL)
models/          ggml-small.en.bin (downloaded, not committed)
vendor/          static ffmpeg (macOS, later) + whisper-rs-sys patch
DESIGN.md        full design & implementation plan
```
