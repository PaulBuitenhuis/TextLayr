use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where we found ffmpeg/ffprobe. Populated once at startup.
#[derive(Debug, Clone)]
pub struct Fftools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
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
    pub has_audio: bool,
    pub pix_fmt: String,
}

/// Locate ffmpeg and ffprobe. Order: bundled Resources/bin -> vendor/<platform> -> PATH.
pub fn find_ffmpeg() -> Result<Fftools> {
    let exe = std::env::current_exe().context("cannot locate current executable")?;
    let exe_dir = exe.parent().unwrap_or(Path::new("."));

    // Candidate dirs, most specific first.
    let mut dirs: Vec<PathBuf> = Vec::new();
    // Bundled .app: Contents/Resources/bin (exe lives in Contents/MacOS).
    if let Some(resources) = exe_dir.parent().map(|d| d.join("Resources").join("bin")) {
        dirs.push(resources);
    }
    // Repo layout: <crate>/vendor/<platform>
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    };
    // Walk up from the exe dir looking for a vendor folder (handles target/debug).
    let mut cur = exe_dir.to_path_buf();
    for _ in 0..5 {
        dirs.push(cur.join("vendor").join(platform));
        if let Some(parent) = cur.parent() {
            cur = parent.to_path_buf();
        } else {
            break;
        }
    }

    let mut ffmpeg = None;
    let mut ffprobe = None;
    for d in &dirs {
        let f = d.join("ffmpeg");
        let p = d.join("ffprobe");
        if ffmpeg.is_none() && f.is_file() {
            ffmpeg = Some(f);
        }
        if ffprobe.is_none() && p.is_file() {
            ffprobe = Some(p);
        }
        if ffmpeg.is_some() && ffprobe.is_some() {
            break;
        }
    }

    // Fall back to PATH.
    if ffmpeg.is_none() {
        ffmpeg = which("ffmpeg");
    }
    if ffprobe.is_none() {
        ffprobe = which("ffprobe");
    }

    let (Some(ffmpeg), Some(ffprobe)) = (ffmpeg, ffprobe) else {
        bail!(
            "ffmpeg/ffprobe not found. Bundle them in the app's Resources/bin, \
             place them in vendor/{}/, or install them on your PATH.",
            platform
        );
    };

    Ok(Fftools { ffmpeg, ffprobe })
}

fn which(bin: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", bin))
        .output()
        .ok()?;
    if out.status.success() {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

/// Probe a video file with ffprobe and return display-oriented properties.
pub fn probe(tools: &Fftools, input: &Path) -> Result<VideoInfo> {
    let out = Command::new(&tools.ffprobe)
        .arg("-v").arg("error")
        .arg("-select_streams").arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height,avg_frame_rate,pix_fmt:stream_side_data=rotation:format=duration")
        .arg("-of").arg("json")
        .arg(input)
        .output()
        .with_context(|| format!("failed to run ffprobe on {}", input.display()))?;

    if !out.status.success() {
        bail!(
            "ffprobe failed for {}: {}",
            input.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("failed to parse ffprobe JSON output")?;

    // --- dimensions + rotation ---
    let stream = json
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .context("no video stream found")?;

    let width = stream
        .get("width")
        .and_then(|v| v.as_u64())
        .context("missing width")? as u32;
    let height = stream
        .get("height")
        .and_then(|v| v.as_u64())
        .context("missing height")? as u32;

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

    // --- audio presence (separate quick query) ---
    let aout = Command::new(&tools.ffprobe)
        .arg("-v").arg("error")
        .arg("-select_streams").arg("a:0")
        .arg("-show_entries").arg("stream=codec_type")
        .arg("-of").arg("csv=p=0")
        .arg(input)
        .output()
        .context("failed to run ffprobe audio query")?;
    let has_audio = aout.status.success()
        && String::from_utf8_lossy(&aout.stdout).trim().starts_with("audio");

    Ok(VideoInfo {
        display_width,
        display_height,
        duration_secs,
        fps,
        has_audio,
        pix_fmt,
    })
}

/// Parse an ffprobe rate string like "30000/1001" or "30/1" into an f64.
fn parse_rate(s: &str) -> Option<f64> {
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
