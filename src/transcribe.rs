use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};

/// A single transcribed word with word-level timing (seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start_secs: f64,
    pub end_secs: f64,
}

/// Transcribe a 16 kHz mono f32le PCM blob into word-level timings using
/// whisper.cpp (small.en). Returns words in chronological order with special
/// tokens (timestamps, blanks, NOSPEECH) filtered out and subword tokens
/// merged into whole words.
pub fn transcribe(model: &Path, samples: &[f32]) -> Result<Vec<Word>> {
    if samples.is_empty() {
        bail!("no audio samples to transcribe");
    }

    // Redirect whisper.cpp/GGML C logs away from stdout (no log backend = dropped).
    whisper_rs::install_logging_hooks();

    // DTW token-level timestamps give accurate per-word timings (the plain
    // token timestamps collapse many words onto the same boundary). DTW runs
    // on CPU/GPU after decode and is well worth the extra compute for karaoke
    // sync. Use the preset for the small English model.
    let mut ctx_params = WhisperContextParameters::new();
    ctx_params.dtw_parameters(DtwParameters {
        mode: DtwMode::ModelPreset {
            model_preset: DtwModelPreset::SmallEn,
        },
        dtw_mem_size: 1024 * 1024 * 128,
    });

    let ctx = WhisperContext::new_with_params(
        model.to_str().context("model path is not valid UTF-8")?,
        ctx_params,
    )
    .context("failed to load whisper model (is the file present and valid?)")?;

    let mut state = ctx.create_state().context("failed to create whisper state")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_token_timestamps(true);
    params.set_language(Some("en"));
    params.set_n_threads(num_cpus());
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);

    state
        .full(params, samples)
        .context("whisper inference failed")?;

    let n_segments = state.full_n_segments().context("full_n_segments failed")?;
    let mut words: Vec<Word> = Vec::new();
    for seg in 0..n_segments {
        let n_tokens = state.full_n_tokens(seg).context("full_n_tokens failed")?;
        for tok in 0..n_tokens {
            let data = state
                .full_get_token_data(seg, tok)
                .context("full_get_token_data failed")?;
            let text = state
                .full_get_token_text_lossy(seg, tok)
                .unwrap_or_default();

            // Filter out non-word tokens: whisper emits timestamp tokens
            // (id < 50364-ish range for multilingual, and special marker tokens)
            // and blank/NOSPEECH. A robust heuristic: skip tokens whose text is
            // empty, whitespace-only, or starts with '[' (e.g. [BLANK],
            // [_NOSPEECH], [_SND_...]) and pure-timestamp tokens.
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed.starts_with('[') || trimmed.starts_with('<') {
                continue;
            }

            // whisper token timestamps are in centiseconds.
            let start_secs = data.t0 as f64 / 100.0;
            let end_secs = data.t1 as f64 / 100.0;

            // Merge subword tokens: whisper prefixes continuation pieces with
            // no leading space, and language-model pieces of English use a
            // leading space for new words. We rebuild words by attaching
            // space-prefixed tokens as new words and non-space ones as suffixes.
            if let Some(last) = words.last_mut() {
                if !text.starts_with(' ') && !text.starts_with(' ') {
                    // continuation of the previous word
                    last.text.push_str(trimmed);
                    last.end_secs = end_secs.max(last.end_secs);
                    continue;
                }
            }

            words.push(Word {
                text: trimmed.to_string(),
                start_secs,
                end_secs,
            });
        }
    }

    // Clean up timing artifacts: spread identical-timestamp clusters evenly,
    // enforce monotonic non-decreasing starts, and a minimum word duration.
    normalize_timings(&mut words, samples.len() as f64 / 16000.0);

    Ok(words)
}

/// Post-process word timings so karaoke highlighting is well-behaved:
///
/// 1. Runs of words that share a timestamp (whisper sometimes collapses a
///    burst of tokens onto one boundary) are spread evenly across the gap to
///    the next distinct timestamp.
/// 2. Starts are forced monotonic (each word starts at/after the previous).
/// 3. Every word gets a small minimum duration so no word is invisible.
///
/// `audio_secs` bounds the final word so we never overshoot the clip end.
fn normalize_timings(words: &mut [Word], audio_secs: f64) {
    if words.is_empty() {
        return;
    }

    // 1. Spread clusters of identical (start) values.
    let mut i = 0;
    while i < words.len() {
        let mut j = i;
        while j + 1 < words.len()
            && (words[j + 1].start_secs - words[i].start_secs).abs() < 1e-6
        {
            j += 1;
        }
        if j > i {
            // This is a cluster from i..=j. The gap it can occupy is from
            // words[i].start to the next distinct word's start (or audio end).
            let cluster_start = words[i].start_secs;
            let cluster_end = if j + 1 < words.len() {
                words[j + 1].start_secs.max(cluster_start)
            } else {
                audio_secs.max(cluster_start)
            };
            let span = cluster_end - cluster_start;
            let n = (j - i + 1) as f64;
            let step = if span > 0.0 { span / n } else { 0.0 };
            for (k, w) in words[i..=j].iter_mut().enumerate() {
                let s = cluster_start + step * k as f64;
                w.start_secs = s;
                w.end_secs = s + step;
            }
        }
        i = j + 1;
    }

    // 2 & 3. Monotonic starts + minimum duration + clamp to audio end.
    const MIN_DUR: f64 = 0.07;
    for idx in 0..words.len() {
        // Start not before previous word's start.
        if idx > 0 && words[idx].start_secs < words[idx - 1].start_secs {
            words[idx].start_secs = words[idx - 1].start_secs;
        }
        // Clamp start to audio length.
        if words[idx].start_secs > audio_secs {
            words[idx].start_secs = audio_secs;
        }
        // Ensure end >= start + MIN_DUR (but not beyond audio end).
        let min_end = (words[idx].start_secs + MIN_DUR).min(audio_secs.max(words[idx].start_secs));
        if words[idx].end_secs < min_end {
            words[idx].end_secs = min_end;
        }
    }
}

fn num_cpus() -> std::ffi::c_int {
    std::thread::available_parallelism()
        .map(|n| n.get() as std::ffi::c_int)
        .unwrap_or(4)
}

// ---------------------------------------------------------------------------
// Transcript cache
//
// whisper small.en costs minutes per run on this machine, so word timings are
// cached per input file (keyed on path + size + mtime) in the platform cache
// dir. Rendering/export iterations then start instantly.
// ---------------------------------------------------------------------------

/// Cache file for `input`, if it can be keyed and the cache dir exists.
pub fn cache_path(input: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(input).ok()?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();

    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    input.to_string_lossy().hash(&mut h);
    len.hash(&mut h);
    mtime.hash(&mut h);

    let dir = dirs::cache_dir()?.join("texxy");
    Some(dir.join(format!("{:016x}.json", h.finish())))
}

/// Load a cached transcript for `input`, if present and parseable.
pub fn load_cached(input: &Path) -> Option<Vec<Word>> {
    let path = cache_path(input)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist a transcript. Best-effort: callers log and continue on error.
pub fn save_cache(input: &Path, words: &[Word]) -> Result<()> {
    let path = cache_path(input).context("cannot derive cache path")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let json = serde_json::to_vec_pretty(words).context("serialize transcript")?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
