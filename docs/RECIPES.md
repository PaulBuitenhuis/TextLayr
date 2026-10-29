# Recipes

The GUI and the CLI run the same pipeline. These recipes cover the part the GUI
can't do: designing a look once, then applying it without opening a window.

All examples assume `textlayr` is on your `PATH`: on macOS re-run the installer
with `--cli`, on Linux symlink the binary or AppImage into `~/.local/bin`. From a
checkout, use `target/release/textlayr` instead. The AppImage also works as the
command itself — `./TextLayr-linux-x86_64.AppImage --list-profiles`.

## Start with a look you made in the app

1. Open a clip in the app and get the style right — the preview updates as you go.
2. **Save as…** in the **Look** dropdown, and give it a name.
3. Find its id:

   ```sh
   textlayr --list-profiles
   ```

   The first column is the id, so a profile saved as "Podcast look" shows up as
   something like `user:podcast-look`.

4. Use it headlessly:

   ```sh
   textlayr ep12.mp4 --headless --profile user:podcast-look
   ```

The app and the CLI read the same store — `~/Library/Application Support/textlayr/profiles/`
on macOS, `~/.config/textlayr/profiles/` on Linux — so there is no export step
and nothing to keep in sync.

`--profile` resolves in this order: an existing `.json` path, a built-in id or
slug (`classic`, `builtin:classic`), a user id (`user:…`), then a display name,
case-insensitively. Prefer the id in scripts: renaming a profile changes its
name but never its id.

`--list-profiles` needs no video and no dependencies, so it's safe to run
anywhere.

## Batch a folder

```sh
mkdir -p out
for f in *.mp4; do
  textlayr "$f" --headless --profile user:podcast -o "out/${f%.mp4}.mp4" \
    || echo "failed: $f" >&2
done
```

- Quote `"$f"` — spaces in filenames are common and the CLI takes them literally.
- With no `-o`, the export lands next to the input as `<name>_textlayr.mp4`.
- An existing output file is refused unless you pass `--force`. The input video
  is never overwritten, `--force` or not.
- The first pass per file is the slow one (whisper). Every later pass over the
  same file hits the transcript cache, so restyling is cheap.

## Keep the look in the repo

Profiles are plain JSON, so a show's look can live next to the show:

```sh
# macOS
ls ~/Library/Application\ Support/textlayr/profiles/

# Linux
ls ~/.config/textlayr/profiles/
```

Copy the `<id>.json` you want next to the show and point `--profile` at it:

```sh
cp ~/.config/textlayr/profiles/<id>.json profiles/podcast.json
textlayr ep12.mp4 --headless --profile profiles/podcast.json
```

A path wins over a profile with the same name, so a pinned file in the repo is
never ambiguous. The file also carries the caption grouping (words per popup),
so the export matches the preview and not just the colours.

## Re-render without re-transcribing

Word timings are cached per input file — keyed on path, size, mtime and the
model — in `~/Library/Caches/textlayr/` on macOS or `~/.cache/textlayr/` on
Linux. That is why restyling an export is quick, and why editing the video, or
switching from `base.en` to `small.en`, transcribes again.

```sh
textlayr ep12.mp4 --headless --profile user:bold-pop      # cache hit
textlayr ep12.mp4 --headless --profile user:bold-pop --no-cache   # force whisper
```

The cache is safe to delete at any time.

## Supply your own timings

`--transcript` skips whisper entirely and reads word timings from JSON:

```json
[
  { "text": "Watch", "start_secs": 0.0, "end_secs": 0.32 },
  { "text": "every", "start_secs": 0.32, "end_secs": 0.61 }
]
```

```sh
textlayr ep12.mp4 --headless --transcript ep12.words.json --profile user:podcast
```

The file is validated before anything is rendered: every word needs text, finite
timestamps, a non-negative start, an end at or after the start, and starts in
ascending order. A typo fails with a readable message instead of a broken export.
Because no model is needed, this is the cheap path for CI and for testing a style
change against fixed text. Cache files use the same shape, so a cached transcript
is also a valid input.

It is headless-only: `--transcript` with `--gui` is an error.

## Stills instead of an export

```sh
textlayr ep12.mp4 --png-dir out/states --profile user:podcast
```

Writes the caption states as PNGs plus the ffmpeg filter script, and stops
before encoding. Useful for checking a font or a shadow at 100%, for design
review, or for reusing the states in another pipeline.

## Encoders

`--encoder auto` (the default) prefers the platform's hardware encoder —
VideoToolbox on macOS, VAAPI on Linux when the probe passes — and falls back to
x264. Name one explicitly when you want a particular result:

```sh
textlayr ep12.mp4 --headless --encoder x264      # slowest, most portable
textlayr ep12.mp4 --headless --encoder videotoolbox
```

`--graph` is the legacy overlay-graph path: higher RAM, and it rejects motion
effects. Leave it alone unless you're debugging the encoder.

## In CI

```sh
textlayr --setup             # fetch ffmpeg + model once; no-op when present
textlayr --check-updates     # 0 up to date · 10 update available · 1 error
textlayr ep12.mp4 --words    # print the (word, start, end) table and stop
```

`--setup` is the prerequisite for every headless run: a fresh machine has no
ffmpeg and no model, and the pipeline needs them. `--words` implies headless and
stops before chunking.

## Gotchas

- Motion effects need the default pipe encoder; `--graph` refuses them.
- `--words-per` and the other style flags override the profile, so
  `--profile user:podcast --words-per 1` means "that look, one word at a time".
- A missing profile fails loudly with `profile not found: …` — check
  `--list-profiles` before blaming the path.
- `--png-dir`, `--words` and `--check-updates` all skip encoding, which makes
  them the fast way to verify a machine is set up correctly.
