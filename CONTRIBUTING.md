# Contributing to TextLayr

TextLayr is a small, focused project: an offline karaoke caption burner for
short-form video. It is maintained by a single author, and **unsolicited pull
requests are closed automatically** — see the
[contribution policy](#contribution-policy) for why, and for what to send
instead.

- [Contribution policy](#contribution-policy)
- [Building from source](#building-from-source)
- [Tests](#tests)
- [Architecture notes](#architecture-notes)
- [Third-party licenses](#third-party-licenses)

## Contribution policy

**Bug reports and feature ideas are welcome. Code contributions are by
invitation only.**

### What to send

| You have | Where it goes |
|---|---|
| A crash, a wrong export, a broken install | [Open a bug report](https://github.com/PaulBuitenhuis/TextLayr/issues/new?template=bug_report.yml) |
| A feature idea or a question | [Discussions](https://github.com/PaulBuitenhuis/TextLayr/discussions) |
| A security problem | [Private advisory](https://github.com/PaulBuitenhuis/TextLayr/security/advisories/new) — never a public issue |
| A patch you want to exist | Fork the repo; MIT lets you publish your own version |

A reproducible bug report is worth more than a patch written against a guess at
the design, so the issue form asks for the version, your OS (the macOS release or
your Linux distro) and the error text. Please fill it in.

### Why no pull requests

TextLayr is a single-author project. Every merged patch becomes code I have to
maintain across ffmpeg, whisper.cpp and future macOS and Linux releases, so
unsolicited PRs are closed automatically. A good bug report is the useful
contribution.

### If you fork

Forking is fine — that's what the licence is for. If you publish a fork, though:

- **Rename it and replace the app icon.** The MIT licence covers the code; it
  doesn't grant trademark rights in the TextLayr name, wordmark or logo, and a
  fork must not imply it is official or endorsed.
- **Don't send users here for support.** Only official releases are supported.
  Bugs that come from a fork's changes will be closed.

## Building from source

TextLayr targets **macOS 13+ on Apple Silicon** and **Linux on x86_64**; Linux is
also the convenient dev environment. Both are published — Linux as a portable
tarball and an AppImage. You need:

| Tool | Why | Notes |
|---|---|---|
| Rust (stable) | build | via [`rustup`](https://rustup.rs) |
| `clang` | builds whisper.cpp | Xcode CLT on macOS, `clang` package on Linux |
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
2. **Fonts** — already bundled in `assets/fonts/` (OFL-licensed); nothing to fetch.

### Build and run

```sh
cargo build --release

# macOS: launch the *bundle*, not the raw binary (macOS runs a bare executable
# through Terminal, which is why the binary pops a terminal window).
scripts/package_macos.sh
open dist/TextLayr.app

# Linux: portable tarball, plus an AppImage when `appimagetool` is on PATH.
# egui/winit need their dev headers first, e.g. on Debian/Ubuntu:
#   sudo apt install libxkbcommon-dev libwayland-dev libx11-dev libxi-dev \
#                    libxcursor-dev libxrandr-dev libxinerama-dev \
#                    libfontconfig1-dev
scripts/package_linux.sh
./dist/TextLayr-<version>-linux-x86_64/textlayr

# GUI straight from a terminal (fine on Linux too); drop zone, or pass a video:
./target/release/textlayr
./target/release/textlayr path/to/video.mp4

# headless pipeline (also selected automatically when any output/style flag is
# present; --gui forces the GUI):
./target/release/textlayr path/to/video.mp4 --headless
```

### The shipped binary's CPU floor

`whisper-rs-sys` builds whisper.cpp through CMake, and CMake enables
`GGML_NATIVE` by default. On a machine with AVX-512 that adds `-march=native`
and bakes those kernels into the binary, so it dies with SIGILL on an older
CPU. The Linux packaging script and the Linux CI jobs therefore export
`GGML_NATIVE=OFF`: ggml keeps its per-ISA defaults (SSE4.2/AVX/AVX2/FMA/F16C/
BMI2 — Intel Haswell 2013, AMD Excavator 2015) and leaves AVX-512* off.

`src/transcribe.rs` refuses to load a model when the CPU is below that floor,
and the Linux CI job disassembles whisper.cpp's static libraries to prove no
AVX-512 kernel leaked in (the linked binary also contains ring's runtime-gated
AVX-512, so it can't be checked as a whole). macOS keeps native codegen: it is
a single ISA family, and a native build there is both faster and still portable
across Apple Silicon. If you build a Linux binary by hand, export
`GGML_NATIVE=OFF` first — or just use `scripts/package_linux.sh`, which does it
for you.

## Tests

```sh
cargo test
```

The suite covers chunking/timing, config round-tripping, preview/export scale
parity, dependency reporting, model metadata, the updater's manifest/version
parsing, and the licenses splitter. It does not download anything or require a
model.

### Test harness

`tests/harness.rs` exercises the real render → encode path without whisper:

- **`--transcript <json>`** feeds a fixed word list
  (`tests/fixtures/transcript-2s.json`) straight into the pipeline, so probing,
  chunking, rendering and encoding all run with no model.
- **`$TEXTLAYR_FFTOOLS`** points ffmpeg/ffprobe discovery (and therefore the
  whole app) at a directory of fixture scripts. `tests/fixtures/fftools/` has
  `hang`, `fail`, `torn-frame`, `hang-decode`, `fail-after-decode`,
  `wrong-frame-size` and a `pass` wrapper that delegates to the real binaries;
  the end-to-end test synthesizes its 2 s clip with `lavfi`.
- The suite asserts the fixtures themselves behave (a hang really hangs, a
  failure exits 3, a torn frame is short), so a later red test is the code's
  fault and not the harness's.
- Anything requiring a real ffmpeg skips with a printed reason when none is
  found. CI installs ffmpeg on both macOS and Linux, so it runs there.

CI runs the gates on macOS and Linux. The Linux job also compiles the
`cfg(target_os = "linux")` branches (VAAPI, PATH lookup) that macOS never sees.

Neither switch is used in normal operation: `$TEXTLAYR_FFTOOLS` is a development
override, and `--transcript` is documented under testing in `--help`.

## Architecture notes

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
- **The social card is generated.** `docs/assets/og.jpg` (1200×630) comes from
  `scripts/make_og.py`, which composes the committed dark banner over the site
  background. Re-run it after changing the banner or the platform line; the card
  is committed so the site deploys without Python.
- **Models are user-managed.** The Settings window lists every supported model with
  its installed state, lets you switch between installed ones, download a
  missing one, and remove a model TextLayr downloaded (`model::remove_downloaded`
  only ever touches TextLayr's own copy in the app data dir — never a bundled,
  `$TEXTLAYR_MODEL` or dev-checkout file). If the stored preference isn't
  installed, TextLayr adopts whichever model is, so the shown choice always equals
  the model in use.
- **The updater is manifest-first.** `update.rs` reads `latest.json` (attached to
  every release) from the version-independent
  `releases/latest/download/latest.json` URL, falling back to the GitHub REST API
  for releases cut before the manifest existed (the fallback is skipped when
  `$TEXTLAYR_UPDATE_URL` is set). Assets are selected from the manifest's
  per-platform `platforms{}` map (keyed by `platform::slug()`), with the legacy
  top-level macOS entries understood for older manifests. Asset URLs resolve as
  *siblings of the manifest*, so the identical file works from GitHub or a local
  server. Downloads are verified against the manifest's SHA-256, and the
  extracted bundle is checked (identifier, version, codesign) before a detached
  helper swaps it in and relaunches. Quarantined "App Translocation" or read-only
  copies fall back to opening the `.dmg` for a manual drag; on Linux the package
  is handed over instead.

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
