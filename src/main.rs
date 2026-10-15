mod audio;
mod model;
mod probe;
mod transcribe;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: texxy <video.mp4>");
        eprintln!();
        eprintln!("Milestone 1 pipeline: probe -> extract audio -> transcribe -> print words.");
        std::process::exit(2);
    };
    let input = PathBuf::from(input);
    if !input.is_file() {
        bail!("input file not found: {}", input.display());
    }

    // 1. ffmpeg discovery + probe
    eprintln!("[1/3] probing video…");
    let tools = probe::find_ffmpeg().context("ffmpeg discovery failed")?;
    eprintln!("      ffmpeg : {}", tools.ffmpeg.display());
    eprintln!("      ffprobe: {}", tools.ffprobe.display());

    let info = probe::probe(&tools, &input).context("probe failed")?;
    eprintln!(
        "      {}x{} (display)  {:.1}s  {:.2} fps  audio={}  pix={}",
        info.display_width,
        info.display_height,
        info.duration_secs,
        info.fps,
        info.has_audio,
        info.pix_fmt
    );
    if !info.has_audio {
        bail!("input has no audio track; cannot transcribe");
    }

    // 2. Extract 16 kHz mono PCM to a temp file
    eprintln!("[2/3] extracting audio…");
    let tmp_dir = tempfile::tempdir().context("temp dir")?;
    let pcm_path = tmp_dir.path().join("audio.f32le");
    let t = Instant::now();
    audio::extract_pcm(&tools, &input, &pcm_path).context("audio extraction failed")?;
    let samples = audio::read_pcm(&pcm_path).context("PCM read failed")?;
    let audio_secs = samples.len() as f64 / 16000.0;
    eprintln!(
        "      {:.1}s of audio extracted in {:.1}s",
        audio_secs,
        t.elapsed().as_secs_f64()
    );

    // 3. Transcribe
    eprintln!("[3/3] transcribing with whisper small.en…");
    let model = model::find_model().context("model discovery failed")?;
    eprintln!("      model: {} ({:.0} MB)", model.display(), model::file_size(&model)? as f64 / 1e6);
    let t = Instant::now();
    let words = transcribe::transcribe(&model, &samples).context("transcription failed")?;
    let elapsed = t.elapsed().as_secs_f64();
    eprintln!(
        "      {} words in {:.1}s  ({:.1}x realtime)",
        words.len(),
        elapsed,
        if elapsed > 0.0 { audio_secs / elapsed } else { 0.0 }
    );

    eprintln!();
    eprintln!("=== transcript (word, start, end) ===");
    for w in &words {
        println!("{:>8.2} {:>8.2}  {}", w.start_secs, w.end_secs, w.text);
    }

    Ok(())
}
