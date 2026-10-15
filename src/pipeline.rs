//! High-level pipeline steps shared by the CLI (`main.rs`) and the GUI (`ui`).
//!
//! Progress is reported through cheap-to-clone `Arc<dyn Fn(..)>` sinks so the
//! same code drives stderr printing in the CLI and a channel in the GUI (the
//! whisper callback runs on the calling thread and requires a `'static` sink).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::chunk::Chunk;
use crate::config::Style;
use crate::probe::{Fftools, VideoInfo};
use crate::render::CaptionState;
use crate::render;
use crate::{audio, export, model, probe, transcribe};

/// Progress of [`ingest`], reported in order.
#[derive(Debug, Clone)]
pub enum IngestProgress {
    /// ffmpeg/ffprobe located; discovery is done.
    Tools(Fftools),
    /// ffprobe pass over the input.
    Probing,
    /// Probe finished.
    Probed(VideoInfo),
    /// Transcript found in cache; transcription skipped.
    Cached(usize),
    /// Audio extraction under way.
    Extracting,
    /// Whisper running; progress is 0..=1.
    Transcribing(f32),
    /// Transcription finished.
    Transcribed {
        words: usize,
        secs: f64,
        realtime_x: f64,
    },
}

/// Progress sink for pipeline steps. `Send` so it can be moved onto a worker
/// thread; ingest bridges it into whisper's `'static` C callback internally.
pub type Progress = Box<dyn FnMut(IngestProgress) + Send>;

/// Everything the review screen needs after a successful ingest.
pub struct Ingest {
    pub tools: Fftools,
    pub info: VideoInfo,
    pub words: Vec<transcribe::Word>,
    pub from_cache: bool,
}

/// Probe the input, then produce word-level timings — from cache when the
/// input file is unchanged and `no_cache` is false.
pub fn ingest(input: &Path, no_cache: bool, progress: Progress) -> Result<Ingest> {
    // Shared with whisper's callback below: the C API wants a `'static` sink,
    // so the caller's closure lives behind a mutex and is invoked in place.
    let progress = Arc::new(std::sync::Mutex::new(progress));
    let report = |ev: IngestProgress| {
        if let Ok(mut f) = progress.lock() {
            f(ev);
        }
    };

    let tools = probe::find_ffmpeg().context("ffmpeg discovery failed")?;
    report(IngestProgress::Tools(tools.clone()));
    report(IngestProgress::Probing);
    let info = probe::probe(&tools, input).context("probe failed")?;
    if !info.has_audio {
        bail!("input has no audio track; cannot transcribe");
    }
    report(IngestProgress::Probed(info.clone()));

    if !no_cache {
        if let Some(cached) = transcribe::load_cached(input) {
            report(IngestProgress::Cached(cached.len()));
            return Ok(Ingest {
                tools,
                info,
                words: cached,
                from_cache: true,
            });
        }
    }

    // Extract 16 kHz mono PCM, then whisper it.
    report(IngestProgress::Extracting);
    let tmp = tempfile::tempdir().context("temp dir")?;
    let pcm_path = tmp.path().join("audio.f32le");
    audio::extract_pcm(&tools, input, &pcm_path).context("audio extraction failed")?;
    let samples = audio::read_pcm(&pcm_path).context("PCM read failed")?;
    let audio_secs = samples.len() as f64 / 16000.0;

    let model = model::find_model().context("model discovery failed")?;
    let sink = Arc::clone(&progress);
    let t = Instant::now();
    let words = transcribe::transcribe(&model, &samples, move |pct| {
        if let Ok(mut f) = sink.lock() {
            f(IngestProgress::Transcribing(pct));
        }
    })
    .context("transcription failed")?;
    let elapsed = t.elapsed().as_secs_f64();
    report(IngestProgress::Transcribed {
        words: words.len(),
        secs: elapsed,
        realtime_x: if elapsed > 0.0 { audio_secs / elapsed } else { 0.0 },
    });
    if let Err(e) = transcribe::save_cache(input, &words) {
        eprintln!("(transcript cache write skipped: {e:#})");
    }

    Ok(Ingest {
        tools,
        info,
        words,
        from_cache: false,
    })
}

/// Progress of [`export_video`].
#[derive(Debug, Clone, Copy)]
pub enum ExportProgress {
    /// Rendering caption state PNGs; 0..=1 over all states.
    Rendering(f32),
    /// ffmpeg encoding; 0..=1 over the output duration.
    Encoding(f32),
}

/// Progress sink for [`export_video`]. Invoked synchronously on the calling
/// thread, so a plain boxed closure is enough (no `'static` bridge needed).
pub type ExportSink = Box<dyn FnMut(ExportProgress) + Send>;

/// Render caption states for `chunks` and burn them into `output`.
///
/// State PNGs live in an internal temp dir that is removed when the export
/// finishes. Encoding goes through the raw-pipe backend (see
/// [`export::export_pipe`]) with `encoder` selecting VAAPI/x264/auto.
pub fn export_video(
    tools: &Fftools,
    input: &Path,
    output: &Path,
    info: &VideoInfo,
    chunks: &[Chunk],
    style: &Style,
    encoder: export::Encoder,
    mut progress: ExportSink,
) -> Result<()> {
    let work = tempfile::tempdir().context("export temp dir")?;
    let states_dir = work.path().join("states");

    let states: Vec<CaptionState> = render::render_states(
        style,
        chunks,
        info.display_width,
        info.display_height,
        &states_dir,
        |frac| progress(ExportProgress::Rendering(frac)),
    )?;

    export::export_pipe(
        tools,
        input,
        output,
        info,
        &states,
        style.position_pct,
        encoder,
        |frac| progress(ExportProgress::Encoding(frac as f32)),
    )?;

    work.close().context("failed to clean up export temp dir")?;
    Ok(())
}

/// Default output path: `<input stem>_karaoke.mp4` next to the input.
pub fn default_output(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string());
    let name = format!("{stem}_karaoke.mp4");
    match input.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}
