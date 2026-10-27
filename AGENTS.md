# TextLayr — agent notes

Read `CONTRIBUTING.md` for policy, building and the public docs. This file is
the short map: where code lives and which files are hot.

## Verify before claiming done

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --no-deps -- -D warnings
cargo test --locked --all-targets
```

## Where to look

Headless core (usable from `main.rs` CLI and the GUI alike):

| Module | Owns |
|---|---|
| `probe.rs` | `Fftools` discovery + `ffprobe` metadata |
| `audio.rs` | ffmpeg → PCM extraction |
| `transcribe.rs` | whisper.cpp, word timings, transcript cache |
| `chunk.rs` | grouping words into caption chunks (one popup per chunk) |
| `config.rs` | `Style`/`Prefs`, font discovery, on-disk persistence |
| `profile.rs` | named saved looks |
| `render/` | caption-state PNGs — see below |
| `export.rs` | ffmpeg compositing (frame pipe by default, `--graph` legacy) |
| `pipeline.rs` | step orchestration + progress types shared with the UI |
| `output.rs` | export destination planning |
| `deps.rs`, `install.rs`, `download.rs`, `paths.rs`, `source.rs` | dependency check, first-run install, downloads, owned dirs |
| `update.rs` | update check and self-install |
| `platform.rs`, `process.rs` | OS integration; bounded child processes |

Front-end:

| Module | Owns |
|---|---|
| `ui/mod.rs` | `App` state, `Msg`, `poll`, `handle`, header, `run` |
| `ui/screens.rs` | full-window screens: drop, install, setup, ingest, export, done |
| `ui/settings.rs` | Settings window: About, Models, Updates, Diagnostics |
| `ui/review.rs` | review screen: transcript editing, style controls |
| `ui/preview.rs` | live video-frame preview |
| `ui/style.rs`, `ui/theme.rs`, `ui/widgets.rs` | style inspector, egui theme, shared stateless widgets |
| `ui/profile.rs`, `ui/licenses.rs` | profile dropdown, embedded license viewer |

`render/` layering is one-way — `layout` and `motion` are leaves, `pixels`
uses both, `metrics` uses `layout`, and `mod.rs` sequences them into an RGBA
buffer or PNG. Keep it that way; a `pixels` → `metrics` edge means the split
has rotted.

## Gotchas that cost the most time

- **State transitions live in `ui/mod.rs`**, next to `handle`. Screens and the
  Settings window only draw and dispatch; they do not own the message flow.
  Adding a transition means touching `Msg` + `handle` + the screen that sends it.
- **Workers report over channels.** whisper/ffmpeg run on threads and push into
  `Msg`; the UI thread never blocks. Don't call slow work from a draw pass.
- **Preview and export share `render_state_rgba`.** A rendering change moves
  both, and `tests/golden_matrix.rs` pins the pixels.
- **Generated, never hand-edit:** `THIRD_PARTY_LICENSES.md`, `assets/icon.png`,
  `AppIcon.icns`, `dmg-background.png`.
- **macOS floor 13** appears in README, the site, `Info.plist` and
  `package_macos.sh` — keep them in sync.
- User-visible changes go under `[Unreleased]` in `CHANGELOG.md`.
