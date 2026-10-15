//! ffmpeg compositing: build the overlay filter graph and run the encode.
//!
//! One `overlay` stage per caption state, each gated by an `enable` window so
//! a state PNG is blended only while its word is being spoken:
//!
//! ```text
//! [0:v][1:v]overlay=x=…:y=…:enable='gte(t,a)*lt(t,b)':eof_action=repeat[v0];
//! [v0][2:v]overlay=…[v1];
//! …
//! [v548][549:v]overlay=…[vout]
//! ```
//!
//! The PNG inputs are still images (no `-loop`): each is decoded exactly once
//! and repeated thereafter via `eof_action=repeat`. The graph is passed to
//! ffmpeg as a file (see [`filter_graph_file_option`]) to sidestep
//! command-length limits.

use crate::probe::{Fftools, VideoInfo};
use crate::render::CaptionState;
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Build the `filter_complex` script for `states`.
///
/// Captions are centered horizontally and placed with the box's vertical
/// center at `position_pct` of the frame height — percentage-based so the same
/// graph works for any resolution and orientation.
pub fn write_filter_script(
    states: &[CaptionState],
    position_pct: f32,
    script_path: &Path,
) -> Result<()> {
    let pos = position_pct as f64 / 100.0;
    let mut s = String::new();
    let mut prev_label = String::from("[0:v]");

    for (i, st) in states.iter().enumerate() {
        let label = format!("[v{i}]");
        let enable = format!(
            "enable='gte(t,{:.4})*lt(t,{:.4})'",
            st.start_secs, st.end_secs
        );
        let last = i + 1 == states.len();
        let out = if last { "[vout]".to_string() } else { label.clone() };
        let sep = if last { "" } else { ";" };
        let idx = i + 1;
        s.push_str(&format!(
            "{prev_label}[{idx}:v]overlay=x=(main_w-overlay_w)/2:y=main_h*{pos:.6}-overlay_h/2:{enable}:eof_action=repeat{out}{sep}\n"
        ));
        prev_label = label;
    }

    std::fs::write(script_path, &s)
        .with_context(|| format!("failed to write {}", script_path.display()))?;
    Ok(())
}

/// Which CLI spelling this ffmpeg build uses to load a filter graph from a file.
///
/// Upstream ffmpeg took `-filter_complex_script <file>`. Newer builds (8.x+,
/// including this machine's ffmpeg n9.0.1) removed it in favor of the generic
/// "option value from file" syntax `-/filter_complex <file>`. Probe the build
/// once per process and cache the answer.
fn filter_graph_file_option(tools: &Fftools) -> &'static str {
    static OPTION: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    *OPTION.get_or_init(|| {
        let legacy = Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-h", "full"])
            .output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|l| l.starts_with("-filter_complex_script"))
            })
            .unwrap_or(false);
        if legacy {
            "-filter_complex_script"
        } else {
            "-/filter_complex"
        }
    })
}

/// Encode `output` with the caption states composited over `input`.
///
/// `on_progress` receives 0.0..=1.0 as the encode advances.
pub fn export(
    tools: &Fftools,
    input: &Path,
    output: &Path,
    info: &VideoInfo,
    states: &[CaptionState],
    position_pct: f32,
    script_path: &Path,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    if states.is_empty() {
        bail!("nothing to render: no caption states");
    }

    write_filter_script(states, position_pct, script_path)?;
    eprintln!(
        "      filter graph: {} overlay stages -> {} ({})",
        states.len(),
        script_path.display(),
        filter_graph_file_option(tools).trim_start_matches('-'),
    );

    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(input);
    for st in states {
        cmd.arg("-i").arg(&st.png);
    }
    cmd.arg(filter_graph_file_option(tools))
        .arg(script_path)
        .arg("-map")
        .arg("[vout]")
        .arg("-map")
        .arg("0:a:0")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("medium")
        .arg("-crf")
        .arg("18")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-progress")
        .arg("pipe:1")
        .arg("-nostats")
        .arg(output);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg")?;

    // Drain stderr on a thread so the pipe can never block the encoder.
    let stderr = child.stderr.take().context("ffmpeg stderr")?;
    let err_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&err_buf);
    let err_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let mut buf = sink.lock().unwrap();
                    buf.extend_from_slice(&line);
                    // Keep only the tail; ffmpeg errors are what matter.
                    const CAP: usize = 16 * 1024;
                    if buf.len() > CAP {
                        let cut = buf.len() - CAP;
                        buf.drain(..cut);
                    }
                }
            }
        }
    });

    // Parse `-progress` on stdout.
    let stdout = child.stdout.take().context("ffmpeg stdout")?;
    let total_secs = info.duration_secs.max(0.001);
    let mut last_frac = 0.0f64;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        // `out_time_ms` is a historical misnomer (it carries microseconds);
        // both keys report the same value, so feeding either through is fine.
        if let Some(v) = line
            .strip_prefix("out_time_us=")
            .or_else(|| line.strip_prefix("out_time_ms="))
        {
            if let Ok(us) = v.trim().parse::<f64>() {
                // Ignore regressions (the muxing tail can wobble the clock).
                if us >= 0.0 {
                    let frac = (us / 1e6 / total_secs).clamp(0.0, 1.0);
                    if frac > last_frac {
                        last_frac = frac;
                        on_progress(frac);
                    }
                }
            }
        } else if line.starts_with("progress=end") {
            last_frac = 1.0;
            on_progress(1.0);
        }
    }

    let status = child.wait().context("failed to wait for ffmpeg")?;
    let _ = err_thread.join();
    on_progress(1.0);

    if !status.success() {
        let tail = String::from_utf8_lossy(&err_buf.lock().unwrap()).to_string();
        bail!(
            "ffmpeg failed ({}): {}",
            status,
            tail.trim()
        );
    }
    if !output.is_file() {
        bail!("ffmpeg reported success but {} is missing", output.display());
    }
    Ok(())
}

/// Grab a single frame at `at_secs` as a PNG (used by the Milestone 3 live
/// preview, and by verification scripts today).
#[allow(dead_code)] // wired into the review screen in Milestone 3
pub fn extract_frame(tools: &Fftools, input: &Path, at_secs: f64, out: &Path) -> Result<()> {
    let status = Command::new(&tools.ffmpeg)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-ss")
        .arg(format!("{at_secs:.3}"))
        .arg("-i")
        .arg(input)
        .arg("-frames:v")
        .arg("1")
        .arg(out)
        .status()
        .context("failed to spawn ffmpeg")?;
    if !status.success() {
        bail!("frame extraction failed at {at_secs:.3}s");
    }
    Ok(())
}
