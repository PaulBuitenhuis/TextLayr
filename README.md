# texxy

Offline karaoke caption burner for short-form video. Drop in a video, review
the transcript, export a new video with word-synced popup captions baked in.

**Design & plan:** see [DESIGN.md](./DESIGN.md).

**Status:** Milestone 3 (interactive egui app + headless CLI). Drop a video in
the GUI, review the transcript, export with the button or **Ctrl+E**; every
stage is also available headlessly.

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
cargo build --release

# GUI: drop zone, or pre-loaded with a video as argv[1]
./target/release/texxy
./target/release/texxy "../samples/AI job search tips.mp4"

# headless (same pipeline; also selected automatically when --words, -o,
# --png-dir, --graph, --encoder or a style flag is present — --gui forces GUI)
./target/release/texxy "../samples/AI job search tips.mp4" --headless
```

Pipeline: `ffprobe` (display dims + rotation, color tags) → ffmpeg audio
extract (16 kHz mono f32le) → whisper transcription with DTW word timings
(cached) → chunking → caption state PNGs → export: ffmpeg decode → Rust
compositing of the state PNGs → ffmpeg encode → `AI job search tips_karaoke.mp4`
(VAAPI H.264 by default, x264 `-crf 18` fallback, audio copied).

```sh
texxy <video> [--words] [--no-cache] [-o out.mp4] [--png-dir dir]
        [--font <family>] [--font-size <pct>] [--position <pct>] [--words-per <n>]
        [--encoder auto|vaapi|x264] [--graph] [--headless] [--gui]
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
| `--encoder` | `auto` (default: VAAPI if `/dev/dri/renderD128` works, else x264) · `vaapi` · `x264` |
| `--graph` | Export via the M2 overlay-filter graph instead of the frame pipe (≈4.2 GB ffmpeg RAM) |
| `--headless` / `--gui` | Force CLI / GUI mode when both would apply |

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
  `ffmpeg -h full` once and uses whichever spelling the installed build accepts
  (graph path; only reachable via `--graph` now).
- **Graph RAM bomb → frame pipe is the default.** The M2 overlay graph passes
  every state as its own ffmpeg input: 754 inputs cost **4.19 GB ffmpeg RSS**,
  which swaps an 8 GB machine to death. `export.rs::export_pipe` instead runs
  ffmpeg decoder → Rust `blend_straight` (lazy 1-entry state-PNG cache) →
  encoder stdin: **453 MB** peak ffmpeg + ~24 MB texxy on the sample.
  `--graph` keeps the old path.
- **VAAPI by default.** `Encoder::Auto` probes `/dev/dri/renderD128` once;
  `h264_vaapi` exports the sample in ~154 s at ~450 MB RSS vs ~1057 s at ~1 GB
  for x264 `-crf 18`. Probe color tags are forwarded so the output matches the
  graph path's tags (`yuvj420p/pc/bt709`).
- **Pipe re-timing uses `avg_frame_rate`, not `r_frame_rate`.** They coincide
  for most CFR files, but the sample's `r_frame_rate` (179/6) is 0.08% off —
  enough to drift video vs audio by ~0.2 s over 222 s. True VFR input would
  need per-frame PTS handling (known limitation).

## Layout

```
src/
  main.rs        arg parsing + headless CLI: probe → transcribe → chunk → render → export
  pipeline.rs    shared orchestration (ingest + export) for CLI and GUI
  probe.rs       ffprobe wrapper (rotation-aware display dims, color tags) + ffmpeg discovery
  audio.rs       16 kHz mono f32le extraction + PCM read
  transcribe.rs  whisper transcription, word merge, timing normalization, cache
  model.rs       whisper model discovery
  chunk.rs       word grouping into caption chunks + highlight windows
  config.rs      Style struct (fonts, colors, sizes) + font discovery
  render.rs      fontdue + tiny-skia -> per-state PNGs + blend_straight (shared with preview)
  export.rs      frame-pipe export (default) + overlay-graph export, encoder selection
  ui/            egui app: drop, ingest, review (preview/editor/style), export, done
assets/fonts/    bundled caption fonts (Inter, Nunito, Libre Baskerville, OFL)
models/          ggml-small.en.bin (downloaded, not committed)
vendor/          static ffmpeg (macOS, later) + whisper-rs-sys patch
DESIGN.md        full design & implementation plan
```
