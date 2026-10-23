use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths;
use crate::platform;
use crate::process;
use crate::source::Source;

/// Where we found ffmpeg/ffprobe. Populated once at startup.
#[derive(Debug, Clone)]
pub struct Fftools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Where the pair came from (bundled / downloaded / system / dev).
    pub source: Source,
}

/// A probed video's essential properties, with rotation already applied so
/// `width`/`height` are the *displayed* dimensions (e.g. the sample's
/// 1920x1080 stored frame with 90deg rotation becomes 1080x1920).
#[derive(Debug, Clone, Serialize)]
pub struct VideoInfo {
    pub display_width: u32,
    pub display_height: u32,
    pub duration_secs: f64,
    pub fps: f64,
    /// Exact frame rate as an ffprobe rational string ("179/6"), preferred for
    /// the raw-pipe export's `-framerate` so durations match the source.
    pub frame_rate: String,
    pub has_audio: bool,
    pub pix_fmt: String,
    /// Color tags from the source stream, verbatim from ffprobe
    /// ("pc"/"tv", "bt709", …). "unknown" when the stream doesn't say —
    /// the pipe export forwards them so re-encoding preserves the signal.
    pub color_range: String,
    pub color_space: String,
    pub color_primaries: String,
    pub color_transfer: String,
}

/// Locate ffmpeg and ffprobe. Order: `$TEXTLAYR_FFTOOLS` -> bundled Resources/bin
/// -> downloaded (app data dir) -> vendor/<platform> (dev) -> PATH.
///
/// `$TEXTLAYR_FFTOOLS` names a *directory* laid out like the bundled
/// `Resources/bin` (an `ffmpeg` and `ffprobe` inside). It exists so the test
/// harness can point the app at fixture scripts; nothing else sets it.
pub fn find_ffmpeg() -> Result<Fftools> {
    let exe = paths::current_exe().context("cannot locate current executable")?;
    let env_dir = std::env::var("TEXTLAYR_FFTOOLS")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    locate(&exe, env_dir.as_deref())
}

/// [`find_ffmpeg`] with the executable path and the override injected, so the
/// precedence rules are testable without mutating the process environment.
fn locate(exe: &Path, env_dir: Option<&Path>) -> Result<Fftools> {
    let dirs = candidate_dirs(exe, env_dir);

    let mut ffmpeg: Option<(PathBuf, Source)> = None;
    let mut ffprobe: Option<(PathBuf, Source)> = None;
    for (d, source) in &dirs {
        let f = d.join(platform::exe_name("ffmpeg"));
        let p = d.join(platform::exe_name("ffprobe"));
        if ffmpeg.is_none() && f.is_file() {
            ffmpeg = Some((f, *source));
        }
        if ffprobe.is_none() && p.is_file() {
            ffprobe = Some((p, *source));
        }
        if ffmpeg.is_some() && ffprobe.is_some() {
            break;
        }
    }

    let vendor = platform::vendor_slug();

    // Fall back to PATH (system-provided).
    if ffmpeg.is_none() {
        ffmpeg = platform::which(&platform::exe_name("ffmpeg")).map(|p| (p, Source::System));
    }
    if ffprobe.is_none() {
        ffprobe = platform::which(&platform::exe_name("ffprobe")).map(|p| (p, Source::System));
    }

    let (Some((ffmpeg, ffmpeg_src)), Some((ffprobe, ffprobe_src))) = (ffmpeg, ffprobe) else {
        bail!(
            "ffmpeg/ffprobe not found. Bundle them in the app's Resources/bin, \
             point $TEXTLAYR_FFTOOLS at a directory holding both, place them in \
             vendor/{vendor}/, install them on your PATH, or use the Setup \
             screen to download them."
        );
    };

    // Report the "weakest" provenance if the two disagree (e.g. bundled ffmpeg
    // but a PATH ffprobe), so the About window never overstates self-containment.
    let source = weaker(ffmpeg_src, ffprobe_src);

    Ok(Fftools {
        ffmpeg,
        ffprobe,
        source,
    })
}

/// Candidate directories, most specific first, each tagged with where it came
/// from.
fn candidate_dirs(exe: &Path, env_dir: Option<&Path>) -> Vec<(PathBuf, Source)> {
    let mut dirs: Vec<(PathBuf, Source)> = Vec::new();
    // Explicit test/dev override wins over everything, so a fixture directory
    // can stand in for the real tools.
    if let Some(dir) = env_dir {
        dirs.push((dir.to_path_buf(), Source::Env));
    }
    // Bundled with the app (the bundle layout differs per OS).
    for (root, source) in platform::bundle_roots_for(Some(exe)) {
        dirs.push((root.join("bin"), source));
    }
    // Downloaded by TextLayr on first run.
    if let Some(bin) = paths::bin_dir() {
        dirs.push((bin, Source::Downloaded));
    }
    // Repo layout: <crate>/vendor/<platform> (dev), walking up from the
    // executable and the current directory.
    let vendor = platform::vendor_slug();
    let cwd = std::env::current_dir().ok();
    for (root, source) in platform::dev_roots_for(Some(exe), cwd.as_deref()) {
        dirs.push((root.join("vendor").join(vendor), source));
    }
    dirs
}

/// Rank provenance so a mixed pair reports the least self-contained source.
fn weaker(a: Source, b: Source) -> Source {
    fn rank(s: Source) -> u8 {
        match s {
            Source::Bundled => 0,
            Source::Downloaded => 1,
            Source::Dev => 2,
            Source::Env => 3,
            Source::System => 4,
        }
    }
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

/// Reject dimensions that cannot produce a sane frame buffer. The upper bound
/// is far beyond any real source (8K is 7680 wide) and keeps `w * h * 4` in
/// range on every supported target.
const MAX_DIMENSION: u64 = 16_384;

fn validate_dimensions(width: u64, height: u64) -> Result<(u32, u32)> {
    if width == 0 || height == 0 {
        bail!("ffprobe reported invalid video dimensions {width}x{height}");
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        bail!("video dimensions {width}x{height} exceed the supported {MAX_DIMENSION}px limit");
    }
    let w = u32::try_from(width).context("video width does not fit in 32 bits")?;
    let h = u32::try_from(height).context("video height does not fit in 32 bits")?;
    Ok((w, h))
}

/// Probe a video file with ffprobe and return display-oriented properties.
pub fn probe(tools: &Fftools, input: &Path) -> Result<VideoInfo> {
    let mut cmd = Command::new(&tools.ffprobe);
    cmd.arg("-v").arg("error")
        .arg("-select_streams").arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height,avg_frame_rate,r_frame_rate,pix_fmt,color_range,color_space,color_primaries,color_transfer:stream_side_data=rotation:format=duration")
        .arg("-of").arg("json")
        .arg(input);
    let out = process::output_with_timeout(&mut cmd, process::PROBE_TIMEOUT)
        .with_context(|| format!("failed to run ffprobe on {}", input.display()))?;

    if !out.status.success() {
        bail!(
            "ffprobe failed for {}: {}",
            input.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("failed to parse ffprobe JSON output")?;

    // --- dimensions + rotation ---
    let stream = json
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .context("no video stream found")?;

    let width = stream
        .get("width")
        .and_then(|v| v.as_u64())
        .context("missing width")?;
    let height = stream
        .get("height")
        .and_then(|v| v.as_u64())
        .context("missing height")?;
    let (width, height) = validate_dimensions(width, height)?;

    // Rotation can appear in stream_side_data (newer ffprobe) or as a top-level
    // tag. Read both.
    let mut rotation = 0i64;
    if let Some(side) = json.get("streams").and_then(|s| s.as_array()) {
        for st in side {
            if let Some(sd) = st.get("side_data_list").and_then(|v| v.as_array()) {
                for entry in sd {
                    if let Some(r) = entry.get("rotation").and_then(|v| v.as_i64()) {
                        rotation = r;
                    }
                }
            }
        }
    }

    // 90 or 270 (either sign) means portrait: swap display dims.
    let normalized = ((rotation % 360) + 360) % 360;
    let (display_width, display_height) = if normalized == 90 || normalized == 270 {
        (height, width)
    } else {
        (width, height)
    };

    // --- fps ---
    let fps = stream
        .get("avg_frame_rate")
        .and_then(|v| v.as_str())
        .and_then(parse_rate)
        .unwrap_or(30.0);

    // Prefer the average rate for re-timing piped frames: for a CFR source it
    // equals nb_frames/duration exactly. `r_frame_rate` is ffprobe's rounded
    // timebase estimate — the sample's "179/6" (29.8333) is off by 0.08% from
    // the true 29.8584, which would drift video vs audio ~0.2 s over 222 s.
    let frame_rate = stream
        .get("avg_frame_rate")
        .and_then(|v| v.as_str())
        .filter(|s| *s != "0/0" && !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            stream
                .get("r_frame_rate")
                .and_then(|v| v.as_str())
                .filter(|s| *s != "0/0" && !s.is_empty())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "30/1".to_string());

    // --- duration (format level) ---
    let duration_secs = json
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let pix_fmt = stream
        .get("pix_fmt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let color = |key: &str| {
        stream
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string()
    };
    let color_range = color("color_range");
    let color_space = color("color_space");
    let color_primaries = color("color_primaries");
    let color_transfer = color("color_transfer");

    // --- audio presence (separate quick query) ---
    let mut aquery = Command::new(&tools.ffprobe);
    aquery
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("a:0")
        .arg("-show_entries")
        .arg("stream=codec_type")
        .arg("-of")
        .arg("csv=p=0")
        .arg(input);
    let aout = process::output_with_timeout(&mut aquery, process::PROBE_TIMEOUT)
        .context("failed to run ffprobe audio query")?;
    let has_audio = aout.status.success()
        && String::from_utf8_lossy(&aout.stdout)
            .trim()
            .starts_with("audio");

    Ok(VideoInfo {
        display_width,
        display_height,
        duration_secs,
        fps,
        frame_rate,
        has_audio,
        pix_fmt,
        color_range,
        color_space,
        color_primaries,
        color_transfer,
    })
}

/// Parse an ffprobe rate string like "30000/1001" or "30/1" into an f64.
pub(crate) fn parse_rate(s: &str) -> Option<f64> {
    if let Some((num, den)) = s.split_once('/') {
        let n: f64 = num.parse().ok()?;
        let d: f64 = den.parse().ok()?;
        if d == 0.0 {
            return None;
        }
        Some(n / d)
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_override_is_the_first_candidate() {
        let exe = Path::new("/Applications/TextLayr.app/Contents/MacOS/TextLayr");
        let dirs = candidate_dirs(exe, Some(Path::new("/fixtures/fftools")));
        assert_eq!(
            dirs.first(),
            Some(&(PathBuf::from("/fixtures/fftools"), Source::Env)),
            "the override must win over the bundle"
        );
        // The bundle is still the next place we look.
        assert_eq!(dirs.get(1).map(|(_, s)| *s), Some(Source::Bundled));
    }

    #[test]
    fn locate_uses_the_override_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = dir.path().join("ffmpeg");
        let ffprobe = dir.path().join("ffprobe");
        std::fs::write(&ffmpeg, b"#!/bin/sh\n").unwrap();
        std::fs::write(&ffprobe, b"#!/bin/sh\n").unwrap();

        let tools = locate(Path::new("/nonexistent/bin/textlayr"), Some(dir.path())).unwrap();
        assert_eq!(tools.ffmpeg, ffmpeg);
        assert_eq!(tools.ffprobe, ffprobe);
        assert_eq!(tools.source, Source::Env);
    }

    #[test]
    fn a_mixed_pair_reports_the_least_self_contained_source() {
        // Bundled ffmpeg + a PATH ffprobe must read as "system (PATH)".
        assert_eq!(weaker(Source::Bundled, Source::System), Source::System);
        assert_eq!(weaker(Source::System, Source::Bundled), Source::System);
        assert_eq!(weaker(Source::Env, Source::Downloaded), Source::Env);
        assert_eq!(weaker(Source::Bundled, Source::Bundled), Source::Bundled);
    }

    #[test]
    fn dimensions_are_validated_before_they_reach_a_buffer() {
        assert_eq!(validate_dimensions(1920, 1080).unwrap(), (1920, 1080));
        assert_eq!(validate_dimensions(16_384, 1).unwrap(), (16_384, 1));
        assert!(validate_dimensions(0, 1080).is_err());
        assert!(validate_dimensions(1920, 0).is_err());
        assert!(validate_dimensions(16_385, 1080).is_err());
        assert!(validate_dimensions(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn rates_parse_or_decline() {
        assert_eq!(parse_rate("30/1"), Some(30.0));
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(
            parse_rate("30000/1001").map(|f| (f * 1000.0).round()),
            Some(29970.0)
        );
        assert_eq!(parse_rate("29.97"), Some(29.97));
        assert_eq!(parse_rate("nope"), None);
    }
}
