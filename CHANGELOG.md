# Changelog

All notable changes to texxy are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  model texxy downloaded. The selected model always matches the one in use.
- **App icon:** a generated texxy icon for the bundle and Dock.
- **Style persistence:** caption style and model choice are saved to the
  platform config directory and restored on launch.
- **Packaging:** `scripts/package_macos.sh` builds an ad-hoc-signed `texxy.app`
  plus `.zip` and `.dmg` release artifacts.

[Unreleased]: https://github.com/PaulBuitenhuis/texxy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/PaulBuitenhuis/texxy/releases/tag/v0.1.0
