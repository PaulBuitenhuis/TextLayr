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

use crate::config::{Anchor, MotionStyle, PopupEffect};
use crate::probe::{parse_rate, Fftools, VideoInfo};
use crate::render::{blend_straight, blend_transformed, CaptionState, MotionPhase};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Top-left y coordinate (in frame pixels) for a caption state whose box
/// anchor should land at `position_pct` of the frame height.
pub fn caption_y(frame_h: u32, position_pct: f32, anchor: Anchor, box_top: f32, box_h: f32) -> f32 {
    let target = frame_h as f32 * position_pct / 100.0;
    match anchor {
        Anchor::Center => target - (box_top + box_h / 2.0),
        Anchor::Top => target - box_top,
        Anchor::Bottom => target - (box_top + box_h),
    }
}

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

/// Device used for VAAPI. Linux-only: probed from `/dev/dri` the first time an
/// encoder is resolved, so a machine whose render node is `renderD129` (or
/// anything else) still gets hardware encoding.
#[cfg(target_os = "linux")]
fn render_nodes(dir: &Path) -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("renderD"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    nodes.sort();
    nodes
}

/// The first render node that passes a one-frame VAAPI probe, once per process.
#[cfg(target_os = "linux")]
fn vaapi_device(tools: &Fftools) -> Option<&'static Path> {
    static DEVICE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DEVICE
        .get_or_init(|| {
            render_nodes(Path::new("/dev/dri"))
                .into_iter()
                .find(|device| vaapi_probe(tools, device))
        })
        .as_deref()
}

/// Probe once per process whether a working VAAPI encoder is present: a render
/// node must exist and a 1-frame test encode must succeed against it.
fn vaapi_works(tools: &Fftools) -> bool {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tools;
        false
    }
    #[cfg(target_os = "linux")]
    {
        vaapi_device(tools).is_some()
    }
}

#[cfg(target_os = "linux")]
fn vaapi_probe(tools: &Fftools, device: &Path) -> bool {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-vaapi_device"])
        .arg(device)
        .args([
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
        ]);
    crate::process::status_with_timeout(&mut cmd, crate::process::PROBE_TIMEOUT)
        .map(|s| s.success())
        .unwrap_or(false)
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
            let mut cmd = Command::new(&tools.ffmpeg);
            cmd.args([
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
            ]);
            crate::process::status_with_timeout(&mut cmd, crate::process::PROBE_TIMEOUT)
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
fn drain_stderr(stream: ChildStderr) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
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

/// Owns a child process and guarantees it is killed and reaped, even on an
/// early `?` or a panic. The child is behind a mutex so a watchdog thread can
/// kill it while the main thread is blocked reading or writing a pipe.
struct ChildGuard {
    child: Arc<Mutex<Child>>,
    err_thread: Option<std::thread::JoinHandle<()>>,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        ChildGuard {
            child: Arc::new(Mutex::new(child)),
            err_thread: None,
            reaped: false,
        }
    }

    fn handle(&self) -> Arc<Mutex<Child>> {
        Arc::clone(&self.child)
    }

    fn take_stdout(&self) -> Option<std::process::ChildStdout> {
        self.child.lock().unwrap().stdout.take()
    }

    fn take_stderr(&self) -> Option<ChildStderr> {
        self.child.lock().unwrap().stderr.take()
    }

    fn take_stdin(&self) -> Option<std::process::ChildStdin> {
        self.child.lock().unwrap().stdin.take()
    }

    fn set_err_thread(&mut self, thread: std::thread::JoinHandle<()>) {
        self.err_thread = Some(thread);
    }

    fn join_err(&mut self) {
        if let Some(thread) = self.err_thread.take() {
            let _ = thread.join();
        }
    }

    /// Poll `try_wait` instead of blocking in `wait`, so the lock is free for
    /// the watchdog's kill.
    fn wait(&mut self) -> Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let status = {
            let mut child = self.child.lock().unwrap();
            child.try_wait().context("failed to poll ffmpeg")?
        };
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn kill(&mut self) {
        if self.reaped {
            return;
        }
        {
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
        }
        let _ = self.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill();
        }
        self.join_err();
    }
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
        let out = if last {
            "[vout]".to_string()
        } else {
            label.clone()
        };
        let sep = if last { "" } else { ";" };
        let idx = i + 1;
        let anchor_offset = match st.anchor {
            Anchor::Center => st.box_top as f64 + st.box_h as f64 / 2.0,
            Anchor::Top => st.box_top as f64,
            Anchor::Bottom => st.box_top as f64 + st.box_h as f64,
        };
        s.push_str(&format!(
            "{prev_label}[{idx}:v]overlay=x=(main_w-overlay_w)/2:y=main_h*{pos:.6}-{anchor_offset:.3}:{enable}:eof_action=repeat{out}{sep}\n"
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
    OPTION.get_or_init(|| {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args(["-hide_banner", "-h", "full"]);
        let legacy = crate::process::output_with_timeout(&mut cmd, crate::process::PROBE_TIMEOUT)
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
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
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
    let (decoded_w, decoded_h) = decoded_frame_size(tools, input)?;
    if (decoded_w, decoded_h) != (info.display_width, info.display_height) {
        bail!(
            "ffprobe reported {}x{} but the decoder produces {decoded_w}x{decoded_h}",
            info.display_width,
            info.display_height
        );
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
        bail!("ffmpeg failed ({}): {}", status, tail.trim());
    }
    if !output.is_file() {
        bail!(
            "ffmpeg reported success but {} is missing",
            output.display()
        );
    }
    Ok(())
}

/// Decode one frame and read its PNG header. ffprobe metadata can disagree with
/// what the decoder actually produces; this is the authoritative size check
/// before a raw RGBA pipe is framed with the metadata's dimensions.
fn decoded_frame_size(tools: &Fftools, input: &Path) -> Result<(u32, u32)> {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ]);
    let out = crate::process::output_with_timeout(&mut cmd, crate::process::PROBE_TIMEOUT)
        .context("failed to decode a frame to verify its size")?;
    if !out.status.success() {
        bail!(
            "could not decode a frame to verify its size: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let bytes = &out.stdout;
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIG {
        bail!("the decoder did not return a PNG frame");
    }
    // IHDR: 8-byte signature, 4-byte length, "IHDR", then width and height.
    let w = u32::from_be_bytes(bytes[16..20].try_into().expect("4 bytes"));
    let h = u32::from_be_bytes(bytes[20..24].try_into().expect("4 bytes"));
    Ok((w, h))
}

/// Encode `output` by piping raw frames through Rust (the default backend).
///
/// ffmpeg decodes `input` to RGBA (rotation applied by autorotate), we blend
/// the active caption state per frame exactly like [`blend_straight`] does in
/// the preview, and a second ffmpeg encodes the composited frames. Peak RAM is
/// one frame + one state, independent of state count.
///
/// `on_progress` receives 0.0..=1.0 as the encode advances. A watchdog watches
/// `cancel` while the pump runs: when it is set, both ffmpeg children are killed
/// (which also unblocks a stalled pipe) and the error says "cancelled".
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
pub fn export_pipe(
    tools: &Fftools,
    input: &Path,
    output: &Path,
    info: &VideoInfo,
    states: &[CaptionState],
    position_pct: f32,
    motion: &MotionStyle,
    encoder: Encoder,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    if states.is_empty() {
        bail!("nothing to render: no caption states");
    }

    let w = info.display_width;
    let h = info.display_height;
    let (decoded_w, decoded_h) = decoded_frame_size(tools, input)?;
    if (decoded_w, decoded_h) != (w, h) {
        bail!("ffprobe reported {w}x{h} but the decoder produces {decoded_w}x{decoded_h}");
    }
    let frame_len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("frame size overflow")?;
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
    let full_range =
        info.color_range == "pc" || info.color_range == "jpeg" || info.pix_fmt.starts_with("yuvj");
    // VideoToolbox cannot pair yuv420p with full range ("Could not get pixel
    // format for color format 'yuv420p' range 'pc'"), so its path always
    // encodes limited range and is tagged tv (see the color-tag loop below).
    let out_range = if full_range && !use_vt {
        "full"
    } else {
        "limited"
    };
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
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-an",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ]);
    let mut dec = ChildGuard::new(
        dec_cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn the decoding ffmpeg")?,
    );
    let dec_out = dec.take_stdout().context("decoder stdout pipe")?;
    let (dec_err, dec_err_thread) = drain_stderr(dec.take_stderr().context("decoder stderr pipe")?);
    dec.set_err_thread(dec_err_thread);

    // ── encoder: composited RGBA frames from stdin -> output ──────────────
    let mut enc_cmd = Command::new(&tools.ffmpeg);
    enc_cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    #[cfg(target_os = "linux")]
    if use_vaapi {
        let device =
            vaapi_device(tools).context("VAAPI was selected but no render node is available")?;
        enc_cmd.arg("-vaapi_device").arg(device);
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
        enc_cmd.args([
            "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", pix,
        ]);
    }
    // Forward color tags so players read the same signal as the source.
    // VideoToolbox can only do limited-range yuv420p, so it is tagged tv.
    let fwd_range = if use_vt {
        "tv"
    } else {
        info.color_range.as_str()
    };
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
        .args([
            "-c:a",
            "copy",
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-nostats",
        ])
        .arg(output);
    let mut enc = ChildGuard::new(
        enc_cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn the encoding ffmpeg")?,
    );
    let mut enc_in = enc.take_stdin().context("encoder stdin pipe")?;
    let enc_out = enc.take_stdout().context("encoder stdout pipe")?;
    let (enc_err, enc_err_thread) = drain_stderr(enc.take_stderr().context("encoder stderr pipe")?);
    enc.set_err_thread(enc_err_thread);

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

    let dec_handle = dec.handle();
    let enc_handle = enc.handle();
    let done = AtomicBool::new(false);

    let (pump, enc_status) = std::thread::scope(|scope| {
        // Cancel must interrupt a blocked read or write, not just be noticed
        // between frames. Killing the children closes their pipes, which wakes
        // the pump below.
        scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    if let Ok(mut child) = dec_handle.lock() {
                        let _ = child.kill();
                    }
                    if let Ok(mut child) = enc_handle.lock() {
                        let _ = child.kill();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
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
                if cancel.load(Ordering::Relaxed) {
                    break Err(anyhow::anyhow!("export cancelled"));
                }
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
                    // expressions: centered, with the box anchor at position_pct.
                    let x = ((w as f32 - *sw as f32) / 2.0).round() as i32;
                    let y =
                        caption_y(h, position_pct, st.anchor, st.box_top, st.box_h).round() as i32;

                    // Popup motion runs across the per-word states of the same
                    // popup. Exit motion wins near the popup's end, so short
                    // popups still leave visibly.
                    let in_dur = motion.duration_ms as f64 / 1000.0;
                    let out_dur = motion.exit_duration_ms as f64 / 1000.0;
                    let phase = if motion.exit != PopupEffect::None
                        && out_dur > 0.0
                        && t >= st.popup_end_secs - out_dur
                    {
                        MotionPhase::Exit(
                            ((t - (st.popup_end_secs - out_dur)) / out_dur).clamp(0.0, 1.0) as f32,
                        )
                    } else if motion.popup != PopupEffect::None
                        && in_dur > 0.0
                        && t < st.popup_start_secs + in_dur
                    {
                        MotionPhase::Entrance(
                            ((t - st.popup_start_secs) / in_dur).clamp(0.0, 1.0) as f32
                        )
                    } else {
                        MotionPhase::Settled
                    };
                    let transform = phase.transform(motion, st.font_px);
                    if transform.is_identity() {
                        blend_straight(&mut buf, w, h, src, *sw, *sh, x, y);
                    } else {
                        let pivot_x = *sw as f32 / 2.0;
                        let pivot_y = match st.anchor {
                            Anchor::Center => st.box_top + st.box_h / 2.0,
                            Anchor::Top => st.box_top,
                            Anchor::Bottom => st.box_top + st.box_h,
                        };
                        blend_transformed(
                            &mut buf, w, h, src, *sw, *sh, x, y, pivot_x, pivot_y, transform,
                        );
                    }
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
        done.store(true, Ordering::Relaxed);
        if pump.is_err() {
            dec.kill();
            enc.kill();
        }
        let enc_status = loop {
            match prog_rx.recv_timeout(Duration::from_millis(100)) {
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
                // Poll rather than block, so a cancel or an early exit is seen
                // even if the progress thread is still draining.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => match enc.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Ok(None) => {}
                    Err(e) => break Err(e),
                },
                // Progress thread finished = encoder exited.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break enc.wait(),
            }
        };
        (pump, enc_status)
    });

    let _ = dec.wait();
    dec.join_err();
    enc.join_err();
    let _ = prog_thread.join();

    // A cancel can land after the decoder reaches EOF but before the rename;
    // never treat that as a successful export.
    if cancel.load(Ordering::Relaxed) {
        bail!("export cancelled");
    }
    let enc_status = enc_status.context("failed to wait for the encoder")?;

    let tail = |buf: &Arc<Mutex<Vec<u8>>>| {
        String::from_utf8_lossy(&buf.lock().unwrap())
            .trim()
            .to_string()
    };
    if let Err(e) = pump {
        // A cancel is a user action, not a failure: keep the message clean so
        // the caller can tell the two apart (and skip the error banner).
        if e.to_string().contains("cancelled") {
            bail!("export cancelled");
        }
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
        bail!(
            "ffmpeg reported success but {} is missing",
            output.display()
        );
    }
    on_progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn placement_uses_the_box_anchor() {
        // box_top = 10, box_h = 20, frame = 1000, position = 50%.
        let (h, pos, top, bh) = (1000, 50.0, 10.0, 20.0);
        assert_eq!(caption_y(h, pos, Anchor::Center, top, bh), 480.0);
        assert_eq!(caption_y(h, pos, Anchor::Top, top, bh), 490.0);
        assert_eq!(caption_y(h, pos, Anchor::Bottom, top, bh), 470.0);
    }

    /// A machine whose render node is not `renderD128` still gets VAAPI; the
    /// walk must ignore card nodes and non-numeric names.
    #[cfg(target_os = "linux")]
    #[test]
    fn render_nodes_are_found_in_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["renderD129", "card0", "renderD128", "renderD", "renderDabc"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let nodes: Vec<PathBuf> = render_nodes(dir.path());
        assert_eq!(
            nodes,
            vec![dir.path().join("renderD128"), dir.path().join("renderD129")]
        );

        let empty = tempfile::tempdir().unwrap();
        assert!(render_nodes(empty.path()).is_empty());
    }

    /// A cancel must interrupt a decoder blocked on a pipe, not just be noticed
    /// between frames. The fixture passes the one-frame size check and then
    /// hangs, exactly like a wedged driver.
    #[cfg(unix)]
    #[test]
    fn cancel_interrupts_a_hung_decoder() {
        let real = match crate::probe::find_ffmpeg() {
            Ok(tools) => tools.ffmpeg,
            Err(e) => {
                eprintln!("skipping: no real ffmpeg available ({e:#})");
                return;
            }
        };
        let work = tempfile::tempdir().unwrap();
        let clip = work.path().join("clip.mp4");
        let status = Command::new(&real)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x203040:s=64x64:r=30:d=1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status()
            .expect("spawn the synthesizing ffmpeg");
        assert!(status.success());

        // Delegate the size check to the real binary, hang on the decoder.
        let wrapper = work.path().join("ffmpeg-hang");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ncase \" $* \" in\n  *\" -frames:v 1 \"*) exec '{}' \"$@\";;\n  *) exec /bin/sleep 120;;\nesac\n",
                real.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&wrapper, perms).unwrap();
        }

        let tools = Fftools {
            ffmpeg: wrapper,
            ffprobe: real,
            source: crate::source::Source::Env,
        };
        let info = VideoInfo {
            display_width: 64,
            display_height: 64,
            duration_secs: 1.0,
            fps: 30.0,
            frame_rate: "30/1".into(),
            has_audio: false,
            pix_fmt: "yuv420p".into(),
            color_range: "tv".into(),
            color_space: "bt709".into(),
            color_primaries: "bt709".into(),
            color_transfer: "bt709".into(),
        };
        let states = vec![CaptionState {
            png: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png"),
            start_secs: 0.0,
            end_secs: 1.0,
            box_top: 0.0,
            box_h: 10.0,
            anchor: Anchor::Center,
            popup_start_secs: 0.0,
            popup_end_secs: 1.0,
            font_px: 10.0,
        }];

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = {
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::Relaxed);
            })
        };
        let started = std::time::Instant::now();
        let err = export_pipe(
            &tools,
            &clip,
            &work.path().join("out.mp4"),
            &info,
            &states,
            50.0,
            &MotionStyle::default(),
            Encoder::X264,
            &cancel,
            |_| {},
        )
        .unwrap_err()
        .to_string();
        cancel_thread.join().unwrap();
        assert!(err.contains("cancelled"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancel took {:?}",
            started.elapsed()
        );
    }
}
