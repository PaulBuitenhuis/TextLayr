use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::probe::Fftools;

/// Extract the audio track as raw 16 kHz mono f32le PCM. Whisper wants 16 kHz
/// mono float samples in [-1, 1]; producing a raw PCM blob means we can read
/// it straight into a `Vec<f32>` with no WAV parsing.
pub fn extract_pcm(tools: &Fftools, video: &Path, pcm: &Path) -> Result<()> {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.arg("-y")
        .arg("-i")
        .arg(video)
        .arg("-vn")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("-f")
        .arg("f32le")
        .arg(pcm);
    // A long video's audio can take a while, but a dead mount must not hang the
    // transcription worker forever.
    let out = crate::process::output_with_timeout(&mut cmd, std::time::Duration::from_secs(600))
        .context("failed to run ffmpeg for audio extraction")?;

    if !out.status.success() {
        bail!(
            "ffmpeg audio extraction failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Read a raw f32le PCM file into samples.
pub fn read_pcm(pcm: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(pcm).context("failed to read PCM file")?;
    let samples = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    Ok(samples)
}
