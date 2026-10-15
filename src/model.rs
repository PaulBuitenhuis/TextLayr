use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Locate the whisper model. Order: bundled Resources/models (in .app) ->
/// <crate>/models/ (dev) -> TEXXY_MODEL env var.
pub fn find_model() -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // .app bundle: Contents/Resources/models
            if let Some(res) = dir.parent().map(|d| d.join("Resources").join("models")) {
                candidates.push(res.join("ggml-small.en.bin"));
            }
            // Walk up to find <crate>/models
            let mut cur = dir.to_path_buf();
            for _ in 0..5 {
                candidates.push(cur.join("models").join("ggml-small.en.bin"));
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
        }
    }

    if let Ok(env_path) = std::env::var("TEXXY_MODEL") {
        candidates.insert(0, PathBuf::from(env_path));
    }

    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }

    bail!(
        "whisper model not found. Looked in: {:?}. Set TEXXY_MODEL or place \
         ggml-small.en.bin in the models/ directory.",
        candidates
    )
}

/// Read a file's size in bytes, for logging.
pub fn file_size(p: &Path) -> Result<u64> {
    let meta = std::fs::metadata(p).context("cannot stat file")?;
    Ok(meta.len())
}
