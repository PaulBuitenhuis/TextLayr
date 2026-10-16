# Contributing to TextLayr

Thanks for taking a look. TextLayr is a small, focused project: an offline karaoke
caption burner for short-form video. Issues and pull requests are welcome.

- [Project layout](#project-layout)
- [Development setup](#development-setup)
- [Building and running](#building-and-running)
- [Tests](#tests)
- [Design notes](#design-notes)
- [Third-party licenses](#third-party-licenses)
- [Packaging and releases](#packaging-and-releases)

## Project layout

```
src/
  main.rs        arg parsing + headless CLI: probe → transcribe → chunk → render → export
  pipeline.rs    shared orchestration (ingest + export) for CLI and GUI
  probe.rs       ffprobe wrapper (rotation-aware display dims, color tags) + ffmpeg discovery
  audio.rs       16 kHz mono f32le extraction + PCM read
  transcribe.rs  whisper transcription, word merge, timing normalization, cache
  model.rs       whisper model identity, discovery, download metadata
  chunk.rs       word grouping into caption chunks + highlight windows
  config.rs      Style struct, font discovery, preference persistence
  deps.rs        startup dependency check + installable-missing description
  download.rs    checksum-verified downloads (curl transport)
  install.rs     first-run installer (ffmpeg/model)
  paths.rs       platform data dirs for downloaded files
  source.rs      provenance of a located dependency (bundled/downloaded/system/…)
  render.rs      fontdue + tiny-skia -> per-state PNGs + blend_straight (shared with preview)
  export.rs      frame-pipe export (default) + overlay-graph export, encoder selection
  ui/            egui app: drop, ingest, review (preview/editor/style), export, done
    licenses.rs  in-app viewer for THIRD_PARTY_LICENSES.md
assets/fonts/    bundled caption fonts (Inter, Nunito, Libre Baskerville, OFL)
assets/icon.png  app icon master (generated; see scripts/make_icon.py)
assets/AppIcon.icns  macOS bundle icon (generated)
assets/dmg-background.png  install-window background (generated)
models/          whisper models (downloaded, not committed; dev only)
scripts/         fetch_model.sh + package_macos.sh + make_dmg.sh + make_icon.py
                 + make_dmg_background.py + gen_third_party_licenses.py
docs/screenshots/  README imagery
vendor/          static ffmpeg (macOS dev) + whisper-rs-sys build patch
```

## Development setup

TextLayr targets **macOS on Apple Silicon**; it also builds and runs on Linux,
which is a convenient dev environment. You need:

| Tool | Why | Notes |
|---|---|---|
| Rust (stable) | build | via [`rustup`](https://rustup.rs) |
| `clang` | builds whisper.cpp | Xcode CLT on macOS |
| `cmake` | whisper-rs builds whisper.cpp | must be on `PATH` |
| `make`, `g++` | C/C++ build | |
| `ffmpeg` + `ffprobe` | probe, audio extract, compositing, encode | system install is fine in dev |

Then:

1. **Model** — fetch one into `models/` for local runs:
   ```sh
   scripts/fetch_model.sh                 # ggml-small.en.bin (~488 MB)
   ```
   Overridable with `$TEXTLAYR_MODEL`; the script also takes an optional
   destination path.
2. **Fonts** — already bundled in `assets/fonts/` (OFL-licensed). To re-fetch,
   see the loop in the repo history / Google Fonts' static-TTF CSS API.

## Building and running

```sh
cargo build --release

# macOS: launch the *bundle*, not the raw binary (macOS runs a bare executable
# through Terminal, which is why the binary pops a terminal window).
scripts/package_macos.sh
open dist/TextLayr.app

# GUI straight from a terminal (fine on Linux too); drop zone, or pass a video:
./target/release/textlayr
./target/release/textlayr path/to/video.mp4

# headless pipeline (also selected automatically when any output/style flag is
# present; --gui forces the GUI):
./target/release/textlayr path/to/video.mp4 --headless
```

## Tests

```sh
cargo test
```

The suite covers chunking/timing, config round-tripping, preview/export scale
parity, dependency reporting, model metadata, and the licenses splitter. It does
not download anything or require a model.

## Design notes

- **Vendored `whisper-rs-sys` patch.** Upstream `whisper-rs-sys` regenerates
  bindgen bindings at build time; on some glibc it emits a `_IO_FILE` layout
  assertion that fails to compile, and an opaque `whisper_full_params`. We
  vendor a copy under `vendor/patch/whisper-rs-sys` (wired via
  `[patch.crates-io]`) using the crate's prebuilt bindings with the layout tests
  stripped. Remove the patch once upstream ships a fix.
- **DTW word timings.** Plain whisper token timestamps collapse several words
  onto one boundary. We enable DTW with the preset for the loaded model
  (`BaseEn`/`SmallEn`) for accurate per-word timings, plus a normalization pass
  that spreads remaining identical-timestamp clusters and enforces a minimum
  word duration.
- **Transcript cache.** whisper costs seconds-to-minutes per run, so word
  timings are cached as JSON in the platform cache dir, keyed on input path +
  size + mtime **and the model** (so switching models never reuses another
  model's timings). `--no-cache` forces re-transcription.
- **Caption states, not subtitles.** Each chunk of N words is rendered once per
  highlighted word — a small RGBA PNG of just the caption box, transparent
  around it. ffmpeg composites each state only during its word's window. Tight
  crops keep ~750 states at ~300 KB instead of ~8 MB each.
- **Still-image inputs, never `-loop`.** A looping image input would re-decode
  the PNG every frame; a plain `-i state.png` decodes once and the overlay
  repeats it (`eof_action=repeat`).
- **Frame pipe is the default export.** The M2 overlay graph passes every state
  as its own ffmpeg input (754 inputs cost ~4.19 GB RSS); `export_pipe` instead
  runs ffmpeg decoder → Rust `blend_straight` → ffmpeg encoder stdin (~453 MB
  peak). `--graph` keeps the old path.
- **Encoder `Auto` prefers hardware.** VideoToolbox on macOS, VAAPI on Linux,
  else x264; each only when its probe passes. VideoToolbox is bitrate-based and
  limited-range, so `--encoder x264` is the way to preserve a full-range
  source's tags exactly.
- **Pipe re-timing uses `avg_frame_rate`.** `r_frame_rate` is ffprobe's rounded
  estimate; on the reference sample it's 0.08% low, enough to drift audio vs
  video ~0.2 s over 222 s. True VFR input would need per-frame PTS handling (a
  known limitation).
- **The build is stamped.** `build.rs` embeds the short git hash; the window
  title and the home screen's corner show `build <hash>`, and `textlayr --version`
  prints it.
- **One theme module drives the look.** `ui/theme.rs` sets the type scale,
  spacing, radii, accent colour, control height and widget palette; screens use
  those builders instead of picking their own sizes.
- **The app icon is generated.** `assets/icon.png` (1024², used at runtime) and
  `assets/AppIcon.icns` (the bundle icon) come from `scripts/make_icon.py`
  (Pillow + numpy + `iconutil`). The `.icns` is *adaptive*: the 16/32px slots
  hold a simplified "t" monogram because the wordmark is unreadable that small.
  Re-run the script after editing the art; both files are committed so builds
  don't need Python.
- **Models are user-managed.** The About window lists every supported model with
  its installed state, lets you switch between installed ones, download a
  missing one, and remove a model TextLayr downloaded (`model::remove_downloaded`
  only ever touches TextLayr's own copy in the app data dir — never a bundled,
  `$TEXTLAYR_MODEL` or dev-checkout file). If the stored preference isn't
  installed, TextLayr adopts whichever model is, so the shown choice always equals
  the model in use.

## Third-party licenses

[`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) is **generated** — don't
edit it by hand. After changing dependencies:

```sh
scripts/gen_third_party_licenses.py
```

It walks `cargo metadata`, collects each dependency's shipped license/notice
files, and emits one deduplicated document (also embedded in the app's About →
"Open source licenses" viewer). The bundled fonts and whisper.cpp texts are
inlined too.

## Packaging and releases

```sh
scripts/package_macos.sh
```

builds `dist/TextLayr.app` and `dist/TextLayr-<version>.{zip,dmg}`. The bundle holds
the binary and fonts only; `ffmpeg`/`probe` and the model are downloaded on first
run. The build is ad-hoc signed and **not notarized**, so first launch on another
Mac needs Right click → Open (or `xattr -dr com.apple.quarantine`).

The `.dmg` is a drag-to-Applications installer: when mounted it shows the app on
the left and an `Applications` alias on the right, with an arrow background.
`scripts/make_dmg.sh` builds it (staging → read-write image → Finder layout via
AppleScript → compressed `UDZO`). That Finder step needs Automation permission
(`Terminal` → `Finder`) on first local run and is skipped gracefully if denied or
if there's no GUI session, leaving a still-installable image with the alias. The
background is generated by `scripts/make_dmg_background.py` and committed.

Releases are cut by pushing a `v*` tag; the `release` GitHub Actions workflow
builds the artifacts and attaches them to a GitHub Release.
