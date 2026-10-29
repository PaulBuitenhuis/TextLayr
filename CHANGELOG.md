# Changelog

All notable changes to TextLayr are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-10-29

### Added

- **Linux (x86_64) is a published platform.** Each release now carries
  `TextLayr-<version>-linux-x86_64.tar.xz` and a matching `.AppImage` beside the
  macOS disk image, with the same version-less aliases
  (`TextLayr-linux-x86_64.tar.xz`, `TextLayr-linux-x86_64.AppImage`). Both are
  self-contained: unpack the tarball and run `./textlayr`, or make the AppImage
  executable and run it. The AppImage needs `libfuse2` (Ubuntu 24.04+:
  `libfuse2t64`); the tarball needs nothing but glibc 2.35+.
- **Linux updates are a hand-off, not a self-replace.** **Download package**
  fetches the shape that is actually installed — the AppImage when the app is
  running as one, otherwise the tarball — verifies its SHA-256 from the release
  manifest, and shows it in your file manager. You swap it in yourself; the
  running copy keeps working.

### Changed

- **Linux packages stay portable across x86-64 CPUs.** whisper.cpp is built with
  `GGML_NATIVE=OFF`, so ggml keeps its per-ISA kernels (SSE4.2, AVX, AVX2, FMA,
  F16C, BMI2) and no AVX-512 leaks in; macOS keeps native codegen. The app
  refuses to load a model on a CPU below the AVX2/FMA floor with a clear message
  instead of dying with SIGILL, and CI disassembles whisper.cpp's objects to
  prove the shipped build stays that way.
- **First-run downloads and updates no longer shell out to `curl` or `ditto`.**

### Fixed

- **Exports work with ffmpeg older than 5.1.** The pipe encoder passed
  `-fps_mode`, which older builds ignore with a warning — the encode then wrote
  audio and no video, without failing. It falls back to `-vsync 0`, and a decoder
  that reports zero frames is now an error rather than an empty export.
- **Videos with a rotation tag keep their orientation on ffprobe older than
  6.0.** Rotation moved from stream side data to stream tags; both are read now.
- **"Show in folder" and "Open in player" work on distros whose `xdg-open` is a
  shell script** that rejects options — it is called with the path alone.
- **A repeat update download keeps a sane name** (`…-x86_64 (1).tar.xz`, not
  `…-x86_64.tar (1).xz`), and a path with a compound archive suffix is preserved.
- **The GTK save dialog's doubled extension is collapsed.** Accepting the
  suggested name used to write `clip_textlayr.mp4.mp4`; it now writes one
  `.mp4`.

## [0.3.0] - 2026-10-23

### Added

- **`--transcript <file>`** (headless) reads word timings from JSON instead of
  running whisper, so the render → export path can be run without a model.
  Intended for tests, CI and debugging a style change; it is the same
  `{text, start_secs, end_secs}` shape as the transcript cache.
- **Open video… on the drop screen.** Pick a video with the native file dialog
  instead of dragging it onto the window. After a failed ingest you can choose
  **Open another…** as well as retrying.
- **Choose where to save an export.** Export now opens the native save panel,
  prefilled with `<name>_textlayr.mp4` in the last folder you used. The finished
  video is written as a hidden `.part.mp4` and only renamed into place on
  success, so a failed or cancelled export cannot destroy an existing file. If
  the name already exists, the panel (macOS) or an in-app confirmation (Linux)
  asks before replacing it.
- **`--force`** for the CLI, to replace an existing output in scripts. It never
  permits overwriting the input video.

### Changed

- **Newer window toolkit (egui/eframe 0.37).** Controls are a little roomier and
  the UI can fall back to system fonts for glyphs the bundled fonts lack.
  Workflow and output are unchanged.
- **The Done screen names the folder** as well as the file, and the chosen
  destination is what **Open video** and **Reveal in Finder** use.
- **Preview popups switch without re-decoding the video.** The preview now
  extracts one 480px frame per popup (instead of a full-resolution PNG per
  click), caches it, and prefetches its neighbours. Clicking a word only moves
  the highlight; the background stays put, and rapid clicks coalesce onto one
  worker instead of piling up ffmpeg processes.

### Fixed

- **Export can no longer destroy a file.** It refuses the input as the output,
  never overwrites an existing file without an explicit confirmation (or
  `--force`), and leaves the previous file intact when an encode fails or is
  cancelled.
- **Cancel interrupts a stalled encode.** The decoder/encoder are killed and
  reaped on every error path, so a wedged ffmpeg cannot leave the app on
  "Cancelling…" forever or leak a child process. Probe and ffprobe waits are now
  bounded.
- **A decoder that disagrees with ffprobe is refused** instead of producing
  mis-framed video, and absurd or overflowing dimensions are rejected before a
  frame buffer is allocated.
- **The updater no longer trusts manifest strings as paths.** A version or asset
  name that could escape its folder is refused, the relaunch helper waits for
  the app to exit instead of swapping under it after 180 s, and a binary
  `Info.plist` is rejected with a clear error.

## [0.2.3] - 2026-10-20

### Added

- **Popup motion, in and out.** A caption can pop, slide or fade in and out,
  with independent entrance/exit effects and durations, easing (linear,
  ease-out, ease-in-out or spring overshoot) and amount. The Motion controls
  play in → hold → out in the live preview. Motion is applied frame-by-frame by
  the default pipe exporter; the legacy `--graph` encoder rejects it with a
  clear message.
- **Wrapping and fixed-size layouts.** The layout model supports a **Fit** mode:
  shrink to one line (the original behaviour), keep the chosen size and wrap, or
  keep the size and clip. Wrapped captions have a line cap, optional words-per-
  line cap, line height, and a **reserve lines** setting so one- and two-line
  popups keep the same box height. Text can be left/centre/right aligned inside
  the box.
- **Fixed boxes and anchors.** A box can use a fixed width and/or height as a
  percentage of the frame, and `position_pct` can refer to the box's top, centre
  or bottom. Caption state PNGs now carry the box's placement inside the crop,
  so the preview and both exporters place every anchor correctly.
- **Caption effects.** Text can now have a solid outline, a soft offset shadow,
  or a coloured glow, all in resolution-independent units. The active word can
  get a rounded pill behind it, an underline, and a static scale-up. The panel
  exposes these as one Edge choice plus a strength; uppercase is a panel toggle,
  while letter tracking and text opacity remain part of the style model for
  profiles.
- **More box control.** The style model supports padding, corner radius, a
  border and the box shadow's colour, opacity, offset and blur. The panel derives
  the common geometry from its Shape, Padding and Shadow controls; all units stay
  relative to the font size, so profiles scale across resolutions.
- **A single-panel style inspector.** The right-hand panel now exposes about
  twenty visual decisions — Look profile dropdown, font, size, uppercase, bold,
  text/accent colours, edge and strength, background none/box/bar, opacity,
  corners, padding, shadow, active-word style, position, width, alignment,
  one-to-three lines, motion type/speed and words-per-popup. Outline width,
  shadow geometry, pill padding, line height, reserved lines and easing are
  derived from those choices, so the full control set no longer needs a sheet.
  There are no floating editor windows or profile gallery; the live preview
  stays visible while styling.
- **Richer built-in profiles.** Bold Pop now demonstrates uppercase, a bold
  outline and a lime active word; Highlight Pill uses the new active-word
  plate; Minimal uses a thin outline; Word by Word uses the active-word scale.
- **Style profiles.** Profiles are a **Look** dropdown, not a gallery: picking
  one applies it immediately. Save, save-as, revert, rename and delete happen
  inline in the panel. Eight built-in looks ship with the app; built-ins are
  read-only. A profile can carry words-per-popup; applying a different caption
  mode offers **Apply both / Look only / Cancel** inline before it re-chunks the
  transcript and discards edits. Profiles are one JSON file each under
  `~/Library/Application Support/textlayr/profiles/`.
- **The CLI can start from a profile.** `--profile <id|path>` loads a built-in,
  saved or hand-written profile, and `--list-profiles` prints what is available.
  `--words-per` now accepts 1–8; 1 is a word-by-word caption mode.
- **The config is grouped and versioned (v3).** Caption style now lives in
  `font`, `text`, `box` and `layout` objects, and words-per-popup moved to a
  separate `grouping` block. Existing v2 configs are migrated on load and
  rewritten once. Every group is defaulted, so later style fields can be added
  without breaking saved configs or profiles.
- **A pixel-regression test for the default look.** The renderer's default style
  is hashed so the migration and future features cannot silently change it.

### Changed

- **The install page is built around the disk image.** It now walks through
  download → drag to Applications → the macOS warning → **Open Anyway** → model
  setup with five numbered steps and pictures (`docs/assets/install/`). The disk
  image window and the Gatekeeper alert are real captures; the Settings step is
  an illustration. The terminal install is still there as the "no security
  prompt" alternative, with a short *how to open Terminal* note.
  A two-page `Install guide.pdf`, generated by `scripts/make_install_guide.py`
  from the same artwork, replaces the text note in the disk image.
- **The minimum is now macOS 13.** `scripts/Info.plist` said 12.0 while the site
  said 13+ and the binary targeted 11.0. 13 is what is advertised and tested;
  12 stays in the backlog until it has been verified on real hardware. The
  packaging script now pins `MACOSX_DEPLOYMENT_TARGET=13.0` too, and the README,
  site, FAQ, DMG guide and packaging output all say 13.
- **Uninstall is documented properly.** The app, the ~250–600 MB of downloaded
  dependencies and the transcript cache are listed separately, the Finder route
  needs no Terminal, and exports are explicitly left alone.
- **`install.sh` gained `--cli` and `--uninstall`.** `--cli` installs a small
  `textlayr` launcher into `/usr/local/bin` (or `~/.local/bin`), and
  `--uninstall` removes the app plus its downloads, preferences and cache after
  showing their sizes. Installs now write a receipt so a custom `--dir` can be
  removed later.
- **The CLI works through a symlink.** `current_exe()` is canonicalized before
  resource lookup, so `textlayr` on `PATH` still finds the fonts, bundled model
  and bundled ffmpeg. Previously a symlink produced "bundled fonts not found".
- **Preferences are backed up.** Every config save keeps the previous v3
  document as `config-v3.bak` beside `config.json`, so an older build that only
  understands v2 cannot silently erase the working style.
- **Settings → Diagnostics can reclaim the download space.** It shows how much
  ffmpeg/models use and can remove them (with confirmation) without touching
  preferences or exported videos.

### Fixed

- The installer no longer rejects an Apple Silicon Mac when Terminal runs under
  Rosetta (`uname -m` says `x86_64` there; it now asks
  `hw.optional.arm64`), accepts `--version v0.2.2`, and reports download
  failures instead of exiting silently.
- The setup screen says the first download needs an internet connection and how
  large it is (~190 MB, or ~530 MB with `small.en`). It now only tells users to
  click **Install**: developer-only hints (`scripts/fetch_model.sh`,
  `$TEXTLAYR_MODEL`, `$TEXTLAYR_FONTS`) are gone from that screen.
- The landing page's "Read the script first" link points at the syntax-highlighted
  GitHub view instead of a file browsers may download, and the README's CLI
  section explains that the app bundle does not put `textlayr` on `PATH`.

## [0.2.2] - 2026-10-18

### Added

- **Support the project.** A `Buy me a coffee` action links to
  [Ko-fi](https://ko-fi.com/U6R0290CU2) from the **Settings** window and from
  the export-complete screen (opened in your browser — nothing is sent from the
  app). The landing page gained a `Support` section (using Ko-fi's in-page
  widget and floating button), a nav link and an FAQ entry; the README gained a
  `Support` section with Ko-fi's badge. Entirely optional.
- **`install.sh`: install from the terminal, with no security prompt at all.**
  Files fetched with `curl` never receive the `com.apple.quarantine` flag that
  browsers and AirDrop attach, so Gatekeeper is never consulted:

  ```sh
  curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh
  ```

  It reads the same `latest.json` manifest the app's updater trusts, verifies the
  download's SHA-256, unpacks with `ditto` (preserving the code signature), and
  installs to `/Applications` or `~/Applications` — never asking for a password.
  Verified before writing any docs: a `curl` download carries no quarantine
  attribute, a quarantined copy is silently refused, and `xattr -dr` restores it.
- **The disk image now carries its own instructions.** `Before you open this.txt`
  (a text file, so it opens with no security prompt) sits in the installer window
  below the drag-to-Applications row, with the one-time Gatekeeper steps and the
  `xattr` fallback; the image's background gained a second line pointing at it.
  The app is also staged with `ditto` now, so its signature always survives
  packaging.

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
- **Installing no longer sends people down a dead end.** Since macOS 15,
  right-click → **Open** does not bypass Gatekeeper, and Homebrew removed the
  `--no-quarantine` escape hatch in late 2025 — but the README, the website and
  the packaging script all still recommended the right-click route, which is why
  a fresh Mac couldn't open the app.

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

[Unreleased]: https://github.com/PaulBuitenhuis/TextLayr/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.4.0
[0.3.0]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.3.0
[0.2.3]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.2.3
[0.2.2]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.2.2
[0.2.1]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.2.1
[0.2.0]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.2.0
[0.1.0]: https://github.com/PaulBuitenhuis/TextLayr/releases/tag/v0.1.0
