//! ffmpeg compositing: caption states burned over the decoded video.
//!
//! Two backends produce the same output:
//!
//! - **pipe** ([`export_pipe`], default): decode → raw RGBA pipe → composite
//!   each frame in Rust (one state PNG decoded lazily on window entry) →
//!   encode. Memory stays O(one frame) regardless of state count, and the
//!   encoder can be VAAPI hardware. This is DESIGN's "Plan B".
//! - **graph** ([`export`], `--graph`): one `overlay` stage per state, each
//!   gated by an `enable` window:
//!
//! ```text
//! [0:v][1:v]overlay=x=…:y=…:enable='gte(t,a)*lt(t,b)':eof_action=repeat[v0];
//! [v0][2:v]overlay=…[v1];
//! …
//! ```
//!
//! The graph opens every state PNG as an input at once — exact PTS handling,
//! but hundreds of decoder + filter contexts in flight (RAM grows with state
//! count), so the pipe is the default.

use crate::probe::{parse_rate, Fftools, VideoInfo};
use crate::render::{blend_straight, CaptionState};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Encoder choice for [`export_pipe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoder {
    /// Best hardware encoder this platform offers, else x264: VAAPI on Linux,
    /// else VideoToolbox on macOS (each used only when its probe passes).
    #[default]
    Auto,
    /// Software x264 (crf 18) — always available, crf-based, and preserves the
    /// source's color range exactly.
    X264,
    /// VAAPI hardware encode (Linux; needs /dev/dri + a working driver).
    Vaapi,
    /// VideoToolbox hardware encode (macOS). Rate-controlled by bitrate (no
    /// crf) and always limited-range (`tv`) — ffmpeg cannot push full-range
    /// yuv420p through VideoToolbox. `Auto` picks it when its probe passes.
    Videotoolbox,
}

/// The encoder [`export_pipe`] actually ends up using after probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Active {
    X264,
    Vaapi,
    VideoToolbox,
}

/// Device used for VAAPI. Linux-only; [`vaapi_works`] checks it first.
#[cfg(target_os = "linux")]
const VAAPI_DEVICE: &str = "/dev/dri/renderD128";

/// Probe once per process whether a working VAAPI encoder is present:
/// the device node must exist and a 1-frame test encode must succeed.
fn vaapi_works(tools: &Fftools) -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = tools;
            false
        }
        #[cfg(target_os = "linux")]
        {
            if !Path::new(VAAPI_DEVICE).exists() {
                return false;
            }
            Command::new(&tools.ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-vaapi_device",
                    VAAPI_DEVICE,
                    "-f",
                    "lavfi",
                    "-i",
                    "color=black:s=256x256:r=30:d=0.2",
                    "-frames:v",
                    "1",
                    "-vf",
                    "format=nv12,hwupload",
                    "-c:v",
                    "h264_vaapi",
                    "-qp",
                    "22",
                    "-f",
                    "null",
                    "-",
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    })
}

/// Probe once per process whether VideoToolbox can encode on this machine.
/// macOS-only; one 1-frame null encode decides it.
fn videotoolbox_works(tools: &Fftools) -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = tools;
            false
        }
        #[cfg(target_os = "macos")]
        {
            Command::new(&tools.ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=black:s=256x256:r=30:d=0.2",
                    "-frames:v",
                    "1",
                    "-c:v",
                    "h264_videotoolbox",
                    "-b:v",
                    "2000k",
                    "-f",
                    "null",
                    "-",
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    })
}

/// Resolve the requested [`Encoder`] to one that actually works here.
///
/// `Auto` prefers the platform's hardware encoder — VAAPI on Linux, else
/// VideoToolbox on macOS — and falls back to x264. A requested hardware
/// encoder that fails its probe silently falls back too, so an export never
/// dies just because a hardware path is unavailable.
fn active_encoder(tools: &Fftools, encoder: Encoder) -> Active {
    match encoder {
        Encoder::X264 => Active::X264,
        Encoder::Vaapi => {
            if vaapi_works(tools) {
                Active::Vaapi
            } else {
                Active::X264
            }
        }
        Encoder::Videotoolbox => {
            if videotoolbox_works(tools) {
                Active::VideoToolbox
            } else {
                Active::X264
            }
        }
        Encoder::Auto => {
            if vaapi_works(tools) {
                Active::Vaapi
            } else if videotoolbox_works(tools) {
                Active::VideoToolbox
            } else {
                Active::X264
            }
        }
    }
}

/// Bitrate for the VideoToolbox path (it has no crf), scaled by resolution and
/// frame rate so the same setting fits 1080p portrait and 4K alike.
fn videotoolbox_bitrate_kbps(w: u32, h: u32, fps: f64) -> u32 {
    let kbps = w as f64 * h as f64 * fps * 0.12 / 1000.0;
    kbps.round().clamp(2_000.0, 40_000.0) as u32
}

/// Map an ffprobe color-matrix name onto `scale`'s `out_color_matrix` value.
/// Unknown/unspecified sources get bt709 (what this project targets).
fn scale_matrix(color_space: &str) -> &'static str {
    match color_space {
        "bt470bg" | "smpte170m" => "bt601",
        "smpte240m" => "smpte240m",
        "fcc" => "fcc",
        "bt2020nc" | "bt2020c" => "bt2020",
        _ => "bt709",
    }
}

/// Drain a child's stderr on a thread, keeping only the tail so a full pipe
/// can never block the child. Returns the shared tail buffer + join handle.
fn drain_stderr(
    stream: ChildStderr,
) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let err_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&err_buf);
    let handle = std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
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
    (err_buf, handle)
}

/// Fill `buf` with exactly one frame; `Ok(false)` = clean EOF at a frame
/// boundary (the expected end of the stream).
fn read_frame(r: &mut impl Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                bail!("decoder ended mid-frame ({filled}/{} bytes)", buf.len());
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("reading a frame from the decoding ffmpeg"),
        }
    }
    Ok(true)
}

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
    let (err_buf, err_thread) = drain_stderr(child.stderr.take().context("ffmpeg stderr")?);

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

/// Encode `output` by piping raw frames through Rust (the default backend).
///
/// ffmpeg decodes `input` to RGBA (rotation applied by autorotate), we blend
/// the active caption state per frame exactly like [`blend_straight`] does in
/// the preview, and a second ffmpeg encodes the composited frames. Peak RAM is
/// one frame + one state, independent of state count.
///
/// `on_progress` receives 0.0..=1.0 as the encode advances.
pub fn export_pipe(
    tools: &Fftools,
    input: &Path,
    output: &Path,
    info: &VideoInfo,
    states: &[CaptionState],
    position_pct: f32,
    encoder: Encoder,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    if states.is_empty() {
        bail!("nothing to render: no caption states");
    }

    let w = info.display_width;
    let h = info.display_height;
    let frame_len = w as usize * h as usize * 4;
    let total_secs = info.duration_secs.max(0.001);
    let rate = if info.frame_rate.is_empty() || info.frame_rate == "0/0" {
        "30/1".to_string()
    } else {
        info.frame_rate.clone()
    };
    let fps = parse_rate(&rate)
        .or(Some(info.fps))
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or(30.0);

    // Honor the source's color tags: full-range sources encode to yuvj420p
    // (what the Milestone 2 graph produced), limited sources to yuv420p.
    let active = active_encoder(tools, encoder);
    let use_vaapi = active == Active::Vaapi;
    let use_vt = active == Active::VideoToolbox;
    let full_range = info.color_range == "pc"
        || info.color_range == "jpeg"
        || info.pix_fmt.starts_with("yuvj");
    // VideoToolbox cannot pair yuv420p with full range ("Could not get pixel
    // format for color format 'yuv420p' range 'pc'"), so its path always
    // encodes limited range and is tagged tv (see the color-tag loop below).
    let out_range = if full_range && !use_vt { "full" } else { "limited" };
    let matrix = scale_matrix(&info.color_space);
    eprintln!(
        "      pipe export: {} states, {} frames total, encoder={} ({out_range} range, {matrix})",
        states.len(),
        (total_secs * fps).round() as u64,
        match active {
            Active::X264 => "x264",
            Active::Vaapi => "vaapi",
            Active::VideoToolbox => "videotoolbox",
        },
    );

    // ── decoder: input -> RGBA frames on stdout ───────────────────────────
    let mut dec_cmd = Command::new(&tools.ffmpeg);
    dec_cmd
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args([
            "-map", "0:v:0", "-fps_mode", "passthrough", "-an", "-f", "rawvideo",
            "-pix_fmt", "rgba", "-",
        ]);
    let mut dec = dec_cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn the decoding ffmpeg")?;
    let dec_out = dec.stdout.take().context("decoder stdout pipe")?;
    let (dec_err, dec_err_thread) = drain_stderr(dec.stderr.take().context("decoder stderr pipe")?);

    // ── encoder: composited RGBA frames from stdin -> output ──────────────
    let mut enc_cmd = Command::new(&tools.ffmpeg);
    enc_cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    #[cfg(target_os = "linux")]
    if use_vaapi {
        enc_cmd.arg("-vaapi_device").arg(VAAPI_DEVICE);
    }
    enc_cmd
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{w}x{h}"))
        .arg("-r")
        .arg(&rate)
        .args(["-i", "pipe:0"])
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-map", "1:a:0"]);
    // `scale` performs rgb->yuv with the source's matrix/range; for VAAPI the
    // following `format=nv12` restricts its output so the conversion happens
    // there too (then `hwupload` moves it to the GPU).
    let vf = if use_vaapi {
        format!("scale=out_color_matrix={matrix}:out_range={out_range},format=nv12,hwupload")
    } else {
        format!("scale=out_color_matrix={matrix}:out_range={out_range}")
    };
    enc_cmd.arg("-vf").arg(&vf);
    if use_vaapi {
        enc_cmd.args(["-c:v", "h264_vaapi", "-qp", "20"]);
    } else if use_vt {
        let kbps = videotoolbox_bitrate_kbps(w, h, fps);
        enc_cmd
            .args(["-c:v", "h264_videotoolbox", "-b:v"])
            .arg(format!("{kbps}k"))
            .args(["-pix_fmt", "yuv420p"]);
    } else {
        let pix = if full_range { "yuvj420p" } else { "yuv420p" };
        enc_cmd.args(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", pix]);
    }
    // Forward color tags so players read the same signal as the source.
    // VideoToolbox can only do limited-range yuv420p, so it is tagged tv.
    let fwd_range = if use_vt { "tv" } else { info.color_range.as_str() };
    for (opt, val) in [
        ("-color_range", fwd_range),
        ("-colorspace", info.color_space.as_str()),
        ("-color_primaries", info.color_primaries.as_str()),
        ("-color_trc", info.color_transfer.as_str()),
    ] {
        if val != "unknown" && !val.is_empty() {
            enc_cmd.arg(opt).arg(val);
        }
    }
    enc_cmd
        .args(["-c:a", "copy", "-movflags", "+faststart", "-progress", "pipe:1", "-nostats"])
        .arg(output);
    let mut enc = enc_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn the encoding ffmpeg")?;
    let mut enc_in = enc.stdin.take().context("encoder stdin pipe")?;
    let enc_out = enc.stdout.take().context("encoder stdout pipe")?;
    let (enc_err, enc_err_thread) = drain_stderr(enc.stderr.take().context("encoder stderr pipe")?);

    // Parse `-progress` on the encoder's stdout on a thread; frames are
    // pumped on this thread, which drains the channel as it goes.
    let (prog_tx, prog_rx) = std::sync::mpsc::channel::<f64>();
    let prog_thread = std::thread::spawn(move || {
        for line in BufReader::new(enc_out).lines().map_while(Result::ok) {
            // `out_time_ms` is a historical misnomer (microseconds either way).
            let v = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="));
            if let Some(v) = v {
                if let Ok(us) = v.trim().parse::<f64>() {
                    if us >= 0.0 {
                        let _ = prog_tx.send(us);
                    }
                }
            } else if line.starts_with("progress=end") {
                let _ = prog_tx.send(f64::MAX); // end sentinel
            }
        }
    });

    // ── pump: read frame → pick state → blend → write ─────────────────────
    let mut buf = vec![0u8; frame_len];
    let mut idx = 0usize;
    let mut cached_idx: Option<usize> = None;
    let mut cached: Option<(Vec<u8>, u32, u32)> = None;
    let mut frame_no = 0u64;
    let mut last_frac = 0.0f64;

    let pump = (|| -> Result<u64> {
        let mut reader = dec_out;
        loop {
            if !read_frame(&mut reader, &mut buf)? {
                break Ok(frame_no);
            }

            // States are chronological and non-overlapping: advance the
            // pointer, then blend if this frame falls inside the window.
            let t = frame_no as f64 / fps;
            while idx < states.len() && t >= states[idx].end_secs {
                idx += 1;
            }
            if let Some(st) = states.get(idx).filter(|s| t >= s.start_secs) {
                if cached_idx != Some(idx) {
                    let img = image::open(&st.png)
                        .with_context(|| format!("failed to read {}", st.png.display()))?
                        .to_rgba8();
                    let (rw, rh) = (img.width(), img.height());
                    cached = Some((img.into_raw(), rw, rh));
                    cached_idx = Some(idx);
                }
                let (src, sw, sh) = cached.as_ref().expect("state cached");
                // Same placement math as the preview and the graph's overlay
                // expressions: centered, box center at position_pct.
                let x = ((w as f32 - *sw as f32) / 2.0).round() as i32;
                let y = (h as f32 * position_pct / 100.0 - *sh as f32 / 2.0).round() as i32;
                blend_straight(&mut buf, w, h, src, *sw, *sh, x, y);
            }

            // Progress reported by the encoder as it consumes our frames.
            while let Ok(us) = prog_rx.try_recv() {
                let frac = if us >= f64::MAX {
                    1.0
                } else {
                    (us / 1e6 / total_secs).clamp(0.0, 1.0)
                };
                if frac > last_frac {
                    last_frac = frac;
                    on_progress(frac);
                }
            }

            enc_in
                .write_all(&buf)
                .map_err(|e| anyhow::anyhow!("encoder pipe closed: {e}"))?;
            frame_no += 1;
        }
    })();

    // End the encoder's input stream (or kill both on failure), then reap.
    drop(enc_in);
    if pump.is_err() {
        let _ = dec.kill();
        let _ = enc.kill();
    }
    let enc_status = loop {
        match prog_rx.recv() {
            Ok(us) => {
                let frac = if us >= f64::MAX {
                    1.0
                } else {
                    (us / 1e6 / total_secs).clamp(0.0, 1.0)
                };
                if frac > last_frac {
                    last_frac = frac;
                    on_progress(frac);
                }
            }
            // Progress thread finished = encoder exited.
            Err(_) => break enc.wait().context("failed to wait for the encoder")?,
        }
    };
    let _ = dec.wait();
    let _ = dec_err_thread.join();
    let _ = prog_thread.join();
    let _ = enc_err_thread.join();
    on_progress(1.0);

    let tail = |buf: &Arc<Mutex<Vec<u8>>>| {
        String::from_utf8_lossy(&buf.lock().unwrap())
            .trim()
            .to_string()
    };
    if let Err(e) = pump {
        bail!(
            "pipe export failed: {e:#}\n  decoder: {}\n  encoder: {}",
            tail(&dec_err),
            tail(&enc_err)
        );
    }
    if !enc_status.success() {
        bail!("ffmpeg encode failed ({enc_status}): {}", tail(&enc_err));
    }
    if !output.is_file() {
        bail!("ffmpeg reported success but {} is missing", output.display());
    }
    Ok(())
}

/// Grab a single frame at `at_secs` as a PNG (used by the GUI's live
/// preview, and by verification scripts).
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
