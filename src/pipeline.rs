//! High-level pipeline steps shared by the CLI (`main.rs`) and the GUI (`ui`).
//!
//! Progress is reported through cheap-to-clone `Arc<dyn Fn(..)>` sinks so the
//! same code drives stderr printing in the CLI and a channel in the GUI (the
//! whisper callback runs on the calling thread and requires a `'static` sink).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::chunk::Chunk;
use crate::config::Style;
use crate::model::{FoundModel, Model};
use crate::probe::{Fftools, VideoInfo};
use crate::render;
use crate::render::CaptionState;
use crate::{audio, export, probe, transcribe};

/// Progress of [`ingest`], reported in order.
#[derive(Debug, Clone)]
pub enum IngestProgress {
    /// ffmpeg/ffprobe located; discovery is done.
    Tools(Fftools),
    /// The whisper model that will be used.
    Model(Model),
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
#[derive(Debug)]
pub struct Ingest {
    pub tools: Fftools,
    pub info: VideoInfo,
    pub words: Vec<transcribe::Word>,
    pub from_cache: bool,
    /// The model that produced these words (or that a cache hit came from).
    pub model: Model,
}

/// Probe the input and report the steps shared by every ingest path.
fn open_input(
    input: &Path,
    model: Option<Model>,
    report: &dyn Fn(IngestProgress),
) -> Result<(Fftools, VideoInfo)> {
    let tools = probe::find_ffmpeg().context("ffmpeg discovery failed")?;
    report(IngestProgress::Tools(tools.clone()));
    if let Some(model) = model {
        report(IngestProgress::Model(model));
    }
    report(IngestProgress::Probing);
    let info = probe::probe(&tools, input).context("probe failed")?;
    if !info.has_audio {
        bail!(
            "this video has no audio track, so there's nothing to transcribe. \
             Karaoke captions need a voiceover."
        );
    }
    report(IngestProgress::Probed(info.clone()));
    Ok((tools, info))
}

/// Probe the input, then produce word-level timings — from cache when the
/// input file is unchanged and `no_cache` is false.
pub fn ingest(
    input: &Path,
    no_cache: bool,
    model: &FoundModel,
    progress: Progress,
) -> Result<Ingest> {
    // Shared with whisper's callback below: the C API wants a `'static` sink,
    // so the caller's closure lives behind a mutex and is invoked in place.
    let progress = Arc::new(std::sync::Mutex::new(progress));
    let report = |ev: IngestProgress| {
        if let Ok(mut f) = progress.lock() {
            f(ev);
        }
    };

    let (tools, info) = open_input(input, Some(model.model), &report)?;

    if !no_cache {
        if let Some(cached) = transcribe::load_cached(input, model.model) {
            report(IngestProgress::Cached(cached.len()));
            return Ok(Ingest {
                tools,
                info,
                words: cached,
                from_cache: true,
                model: model.model,
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

    let kind = model.model;
    let model_path = model.path.clone();
    let sink = Arc::clone(&progress);
    let t = Instant::now();
    let words = transcribe::transcribe(kind, &model_path, &samples, move |pct| {
        if let Ok(mut f) = sink.lock() {
            f(IngestProgress::Transcribing(pct));
        }
    })
    .context("transcription failed")?;
    let elapsed = t.elapsed().as_secs_f64();
    report(IngestProgress::Transcribed {
        words: words.len(),
        secs: elapsed,
        realtime_x: if elapsed > 0.0 {
            audio_secs / elapsed
        } else {
            0.0
        },
    });
    if let Err(e) = transcribe::save_cache(input, kind, &words) {
        eprintln!("(transcript cache write skipped: {e:#})");
    }

    Ok(Ingest {
        tools,
        info,
        words,
        from_cache: false,
        model: kind,
    })
}

/// [`ingest`] with word timings read from `transcript` instead of whisper.
///
/// Backs the `--transcript` flag: probing, chunking, rendering and exporting are
/// the real code paths, so a style or export change can be exercised without a
/// model or a multi-minute transcription. Nothing is written to the cache.
pub fn ingest_from_file(input: &Path, transcript: &Path, progress: Progress) -> Result<Ingest> {
    let progress = Arc::new(std::sync::Mutex::new(progress));
    let report = |ev: IngestProgress| {
        if let Ok(mut f) = progress.lock() {
            f(ev);
        }
    };

    // Read (and validate) the transcript first: a typo'd path should fail
    // before we spend time probing, and it makes the error the useful one.
    let words = transcribe::read_words(transcript).context("transcript file")?;
    let (tools, info) = open_input(input, None, &report)?;

    Ok(Ingest {
        tools,
        info,
        words,
        from_cache: false,
        model: Model::default(),
    })
}

/// Which pass of the export is running.
///
/// The export is two passes: rendering one PNG per caption state, then burning
/// them in (video + audio) through ffmpeg. Each reports its own 0..=1, and the
/// GUI shows them as separate rows so a pass change never looks like the bar
/// jumping backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPhase {
    /// Rendering caption state PNGs; one per (chunk, highlighted word).
    Rendering,
    /// ffmpeg encoding; 0..=1 over the output duration.
    Encoding,
}

impl ExportPhase {
    /// The passes in the order they run.
    pub const ALL: [ExportPhase; 2] = [ExportPhase::Rendering, ExportPhase::Encoding];

    /// Whether this pass runs before `other`.
    pub fn before(self, other: ExportPhase) -> bool {
        ExportPhase::ALL.iter().position(|p| *p == self)
            < ExportPhase::ALL.iter().position(|p| *p == other)
    }

    /// Short user-facing name for the pass.
    pub fn label(self) -> &'static str {
        match self {
            ExportPhase::Rendering => "Render captions",
            ExportPhase::Encoding => "Encode video",
        }
    }
}

/// Share of the overall progress the render pass accounts for. Rendering
/// hundreds of PNGs is quick next to a full-length encode.
const RENDER_WEIGHT: f32 = 0.25;

/// Progress of [`export_video`].
#[derive(Debug, Clone, Copy)]
pub struct ExportProgress {
    /// Which pass this update is about.
    pub phase: ExportPhase,
    /// How far through the current pass, 0..=1.
    pub phase_frac: f32,
    /// How far through the whole export, 0..=1. Monotonic, and the only number
    /// a time estimate should be derived from.
    pub overall: f32,
}

impl ExportProgress {
    fn new(phase: ExportPhase, phase_frac: f32) -> Self {
        let phase_frac = phase_frac.clamp(0.0, 1.0);
        let overall = match phase {
            ExportPhase::Rendering => phase_frac * RENDER_WEIGHT,
            ExportPhase::Encoding => RENDER_WEIGHT + phase_frac * (1.0 - RENDER_WEIGHT),
        };
        ExportProgress {
            phase,
            phase_frac,
            overall,
        }
    }
}

/// Progress sink for [`export_video`]. Invoked synchronously on the calling
/// thread, so a plain boxed closure is enough (no `'static` bridge needed).
pub type ExportSink = Box<dyn FnMut(ExportProgress) + Send>;

/// Render caption states for `chunks` and burn them into `output`.
///
/// State PNGs live in an internal temp dir that is removed when the export
/// finishes. Encoding goes through the raw-pipe backend (see
/// [`export::export_pipe`]) with `encoder` selecting VAAPI/x264/auto.
///
/// `cancel` is polled between states and while the encode runs; when it is set
/// the ffmpeg children are killed and the error message contains "cancelled" so
/// callers can tell a cancel from a failure. The caller owns the destination
/// (see [`crate::output::ExportTarget`]) and removes its part file on error.
#[allow(clippy::too_many_arguments)] // a pipeline step: each parameter is distinct
pub fn export_video(
    tools: &Fftools,
    input: &Path,
    output: &Path,
    info: &VideoInfo,
    chunks: &[Chunk],
    style: &Style,
    encoder: export::Encoder,
    cancel: &AtomicBool,
    mut progress: ExportSink,
) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("export cancelled");
    }
    let work = tempfile::tempdir().context("export temp dir")?;
    let states_dir = work.path().join("states");

    let states: Vec<CaptionState> = render::render_states(
        style,
        chunks,
        info.display_width,
        info.display_height,
        &states_dir,
        cancel,
        |frac| progress(ExportProgress::new(ExportPhase::Rendering, frac)),
    )?;

    let encoded = export::export_pipe(
        tools,
        input,
        output,
        info,
        &states,
        style.layout.position_pct,
        &style.motion,
        encoder,
        cancel,
        |frac| progress(ExportProgress::new(ExportPhase::Encoding, frac as f32)),
    );

    // The caller owns the destination. Encoders write to an
    // [`output::ExportTarget`] part file, so a failure or cancel here must not
    // delete anything the caller did not create.
    encoded?;

    work.close().context("failed to clean up export temp dir")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overall number is what the KTA (and the ETA) is derived from, so it
    /// must climb across both passes and never dip when they hand over.
    #[test]
    fn overall_progress_is_monotonic_across_passes() {
        let render: Vec<f32> = [0.0, 0.25, 0.5, 0.75, 1.0]
            .iter()
            .map(|f| ExportProgress::new(ExportPhase::Rendering, *f).overall)
            .collect();
        assert_eq!(render[0], 0.0);
        assert!((render.last().copied().unwrap() - RENDER_WEIGHT).abs() < 1e-6);

        let encode: Vec<f32> = [0.0, 0.5, 1.0]
            .iter()
            .map(|f| ExportProgress::new(ExportPhase::Encoding, *f).overall)
            .collect();
        assert!(
            (encode[0] - RENDER_WEIGHT).abs() < 1e-6,
            "handover is seamless"
        );
        assert!((encode.last().copied().unwrap() - 1.0).abs() < 1e-6);

        let all: Vec<f32> = render.iter().chain(encode.iter()).copied().collect();
        assert!(all.windows(2).all(|w| w[1] >= w[0]), "{all:?}");
    }

    /// Out-of-range fractions (a stalled encoder reporting past the duration)
    /// must be clamped rather than producing >100% overall.
    #[test]
    fn overall_progress_clamps() {
        assert_eq!(ExportProgress::new(ExportPhase::Encoding, 1.5).overall, 1.0);
        assert_eq!(
            ExportProgress::new(ExportPhase::Rendering, -1.0).overall,
            0.0
        );
    }

    /// A `--transcript` path that cannot be read fails on the transcript, not
    /// on a probe, and does it before any ffmpeg work.
    #[test]
    fn a_missing_transcript_fails_before_probing() {
        let dir = tempfile::tempdir().unwrap();
        let err = ingest_from_file(
            Path::new("/nonexistent/video.mp4"),
            &dir.path().join("nope.json"),
            Box::new(|_| {}),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("transcript file"), "{msg}");
        assert!(msg.contains("nope.json"), "{msg}");
    }
}
