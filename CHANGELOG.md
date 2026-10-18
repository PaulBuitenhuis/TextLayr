# Changelog

All notable changes to TextLayr are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Support the project.** A `Buy me a coffee` action links to
  [Ko-fi](https://ko-fi.com/U6R0290CU2) from the **Settings** window and from
  the export-complete screen (opened in your browser — nothing is sent from the
  app). The landing page gained a `Support` section (using Ko-fi's in-page
  widget and floating button), a nav link and an FAQ entry; the README gained a
  `Support` section with Ko-fi's badge. Entirely optional.

### Changed

- **The About window is now called Settings**, and its header button matches:
  `Settings`, also reachable with **⌘,**. It is a small, tabbed panel (About /
  Models / Updates / Diagnostics) instead of one long column, bounded to the
  viewport so nothing is cut off on shorter displays; the whisper-model controls,
  update settings and resolved runtime paths each get their own tab.
- **The header bar is a fixed height** with its contents centred in it. It
  previously sized itself from the first frame it drew, which left the wordmark
  and the button hugging the top with dead space beneath.
- **The export shows its passes.** Rendering captions and encoding video each get
  their own row with their own bar (plus the caption-state count), so progress
  never looks like it jumps backwards at the hand-off; an approximate time
  remaining appears once there is enough signal, and a **Cancel** button stops
  the export, removes the half-written file and returns to Review with every edit
  intact — no error banner.
- **The About tab shows the wordmark** used on the main screen instead of a
  square icon, and the support ask now sits in its own card rather than under a
  divider.
- **Models download in place.** Choosing a model no longer closes the Settings
  window and bounces you to the main screen — progress shows inline and can be
  cancelled there. Each model is now its own block with its own buttons
  (**Use** / **Remove** / **Download**) directly beneath it, so no action can be
  mistaken for another model's; removing asks *“Remove small.en?”* first.
  Removable copies are the ones TextLayr manages (its downloads and a dev
  checkout's `models/*.bin`); app-bundled or env-var models instead offer
  **Reveal in Finder**.
- **Diagnostics are readable.** The resolved paths are shown in full (wrapped,
  selectable) with copy/reveal actions instead of being truncated to a tooltip,
  and a **Copy diagnostics report** button copies a plain-text summary for bug
  reports.
- **The Open source licenses window opens at a usable size** and can be resized
  (its section list is a draggable sidebar).
- **The export-complete screen now shows what you made.** The finished frame
  (captions and all) is the hero, clickable to play, above a metadata line
  (`1280×720 · 1:00 · 212 MB · exported in 18s`); there is one filled **Open
  video** button with **Show in Finder** and **New video** as muted secondary
  buttons beneath it. The block is a fixed-width column centred in the window, so
  it no longer stretches across a wide one. The app also credits the developer as
  *"Made with ❤ by Paul Buitenhuis"*.

### Fixed

- **Crash when opening a second video after an export.** **New video** (or
  **Change video**) invalidated the in-flight preview-frame generation by parking
  its counter at `u64::MAX`, and the next frame request added one to it — an
  overflow panic in debug builds. The counter now wraps.

## [0.2.1] - 2026-10-17

### Added

- **In-app updates.** TextLayr reads a small `latest.json` manifest attached to
  the newest GitHub release (via the stable
  `releases/latest/download/latest.json` URL), falling back to the GitHub REST
  API for releases cut before the manifest existed. It checks at most once a day
  (toggleable in **About → Updates**), shows an **Update available** button, and
  can download, verify, swap its own bundle in place and relaunch. When
  self-update isn't possible — a quarantined/translocated or read-only copy — it
  downloads the `.dmg` and opens it for a manual drag instead. Downloads are
  verified against the SHA-256 in the manifest, and the extracted app is checked
  for the expected bundle identifier, version and codesign signature before
  anything is replaced.
- **`--check-updates`** CLI flag: report whether a newer release exists and exit
  (0 up to date, 10 update available, 1 error) for scripting.
- **`scripts/serve_updates_local.sh`** and the `$TEXTLAYR_UPDATE_URL` /
  `$TEXTLAYR_UPDATE_FORCE` / `$TEXTLAYR_UPDATE_SKIP_CODESIGN` overrides, so the
  updater can be exercised against a local fake release without publishing.

### Changed

- The release workflow also publishes a `latest.json` update manifest (alongside
  `SHA256SUMS`).
- Preferences persist the update-check settings. Existing config files load
  unchanged; automatic update checks default to on and can be switched off.

## [0.2.0] - 2026-10-16

The project is now **TextLayr** (formerly *texxy*), renamed to avoid confusion
with an unrelated product that uses the old name.

### Changed

- **Renamed** from `texxy` to `textlayr` throughout: the crate and CLI binary,
  the macOS bundle (`TextLayr.app`, identifier `com.paulbuitenhuis.textlayr`),
  the app data/config/cache directories, and the `TEXTLAYR_*` environment
  variables. Existing `texxy` data directories are migrated on first launch, so
  a downloaded model and saved preferences carry over.
- **Dependency updates:** `whisper-rs` 0.14 → 0.16 (re-vendored `whisper-rs-sys`
  0.15), `sha2` 0.10 → 0.11, `tiny-skia` 0.11 → 0.12, `dirs` 6 → 7.
- **Output name:** the default export is now `<input>_textlayr.mp4` (was
  `<input>_karaoke.mp4`).

## [0.1.0] - 2026-10-16

The first public release: an offline karaoke caption burner for short-form
video, as a native macOS app plus a headless CLI.

### Added

- **GUI (egui):** drag-and-drop, threaded ingest/export with progress, a
  transcript editor, a live composited preview, and a full caption style panel.
- **Headless CLI:** the same pipeline with a word-timing dump mode (`--words`),
  style overrides, and encoder selection.
- **Word-synced captions:** whisper.cpp transcription with DTW word timings,
  punctuation-aware chunking, and per-word highlight windows.
- **Custom renderer:** fontdue + tiny-skia produce tight-crop RGBA caption
  states, composited by a Rust frame-pipe export (hardware H.264 via
  VideoToolbox on macOS, x264 fallback).
- **First-run setup:** downloads `ffmpeg`/`ffprobe` (if not already on `PATH`)
  and a whisper model into the app data directory, with pinned SHA-256 checks.
  `base.en` is the default; `small.en` is an optional upgrade.
- **Model management:** the About window shows each model's installed state,
  lets you switch between installed models, download a missing one, and remove a
  model TextLayr downloaded. The selected model always matches the one in use.
- **App icon:** a generated TextLayr icon for the bundle and Dock.
- **Style persistence:** caption style and model choice are saved to the
  platform config directory and restored on launch.
- **Packaging:** `scripts/package_macos.sh` builds an ad-hoc-signed `TextLayr.app`
  plus `.zip` and `.dmg` release artifacts.

[Unreleased]: https://github.com/PaulBuitenhuis/TextLayr/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.2.1
[0.2.0]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.2.0
[0.1.0]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.1.0
