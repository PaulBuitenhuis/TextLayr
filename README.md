# texxy

Offline karaoke caption burner for short-form video. Drop in a video, review
the transcript, export a new video with word-synced popup captions baked in.

**Design & plan:** see [DESIGN.md](./DESIGN.md).

**Status:** Milestone 1 (audio + transcription pipeline). GUI and export come in
later milestones.

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
4. **Fonts** — `assets/fonts/` is reserved for Inter, Nunito, and Libre Baskerville
   (Milestone 2+). Not yet populated.

## Build & run (Milestone 1 CLI)

```sh
cargo build
cargo run -- "../samples/AI job search tips.mp4"
```

Pipeline: `ffprobe` (display dims + rotation) → ffmpeg audio extract (16 kHz mono
f32le) → whisper transcription with DTW word timings → prints `(start, end, word)`
table.

Suppress progress on stderr, keep the table on stdout:

```sh
./target/debug/texxy video.mp4 2>/dev/null
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

## Layout

```
src/
  main.rs        M1 CLI pipeline
  probe.rs       ffprobe wrapper (rotation-aware display dims) + ffmpeg discovery
  audio.rs       16 kHz mono f32le extraction + PCM read
  transcribe.rs  whisper transcription, word merge, timing normalization
  model.rs       whisper model discovery
assets/fonts/    (Milestone 2+) bundled caption fonts
models/          ggml-small.en.bin (downloaded, not committed)
vendor/          static ffmpeg (macOS, later) + whisper-rs-sys patch
DESIGN.md        full design & implementation plan
```
