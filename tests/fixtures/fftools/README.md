# ffmpeg/ffprobe fixtures

Fake tools for the test harness, laid out like the app's bundled
`Contents/Resources/bin` (an `ffmpeg` and an `ffprobe` in one directory). Point
`$TEXTLAYR_FFTOOLS` at one of these directories to make the app use it instead
of the bundled, downloaded, vendored or `PATH` copy.

| Directory | Behaviour |
|---|---|
| `hang/` | Never exits. Proves cancel/timeout handling without needing a wedged real ffmpeg. |
| `fail/` | Exits 3 immediately, with a `textlayr-fixture:` line on stderr. |
| `torn-frame/` | Exits 0 after writing 100 bytes — a decoder killed mid-frame. |
| `pass/` | Delegates to the real tool (`$TEXTLAYR_REAL_FFMPEG` / `$TEXTLAYR_REAL_FFPROBE`, else `PATH`), appending each invocation to `$TEXTLAYR_FFTOOLS_LOG` when set. |

Notes:

- The scripts are committed executable; `tests/harness.rs` asserts that and
  fails with a `chmod +x` hint if a checkout loses the bit.
- `pass/` must not loop: fixture directories are never on `PATH`, so its
  `command -v ffmpeg` resolves the genuine binary.
- `$TEXTLAYR_FFTOOLS` is a *development and test* override. Nothing in normal
  use sets it, and the Diagnostics tab reports the tools as "env var" when it
  is active.
