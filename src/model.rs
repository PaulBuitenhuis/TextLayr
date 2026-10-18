//! Whisper model identity, discovery and download metadata.
//!
//! TextLayr uses an English-only whisper.cpp model. `base.en` is the default
//! (~148 MB — quick to fetch on first run); `small.en` (~488 MB) is the
//! optional accuracy upgrade. Each model has a fixed download URL (pinned to a
//! HuggingFace revision so it can never change under us) and a SHA-256.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::paths;
use crate::source::Source;

/// A supported whisper model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Model {
    /// First run and the CLI: the small, fast model.
    #[default]
    #[serde(rename = "base.en")]
    BaseEn,
    #[serde(rename = "small.en")]
    SmallEn,
}

impl Model {
    /// Every model, in menu order (fastest first).
    pub const ALL: [Model; 2] = [Model::BaseEn, Model::SmallEn];

    /// Stable id, used in the config file and cache keys.
    pub fn id(self) -> &'static str {
        match self {
            Model::BaseEn => "base.en",
            Model::SmallEn => "small.en",
        }
    }

    /// File name on disk.
    pub fn file_name(self) -> &'static str {
        match self {
            Model::BaseEn => "ggml-base.en.bin",
            Model::SmallEn => "ggml-small.en.bin",
        }
    }

    /// One-line description for the setup screen.
    pub fn label(self) -> &'static str {
        match self {
            Model::BaseEn => "base.en — fast, ~148 MB (recommended)",
            Model::SmallEn => "small.en — more accurate, ~488 MB",
        }
    }

    /// Approximate download size, for progress and messaging.
    pub fn approx_bytes(self) -> u64 {
        match self {
            Model::BaseEn => 147_964_211,
            Model::SmallEn => 487_614_201,
        }
    }

    /// SHA-256 of the model file, from the HuggingFace LFS listing. Downloads
    /// are rejected unless they match, so a corrupt or substituted file can
    /// never be loaded.
    pub fn sha256(self) -> &'static str {
        match self {
            Model::BaseEn => "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
            Model::SmallEn => "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        }
    }

    /// Immutable download URL (pinned to a HuggingFace commit, so the bytes
    /// behind it can never change).
    pub fn download_url(self) -> &'static str {
        match self {
            Model::BaseEn => {
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin"
            }
            Model::SmallEn => {
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.en.bin"
            }
        }
    }

    /// Parse a model id ("base.en"/"small.en"), as stored in the config.
    pub fn from_id(id: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// A located model: which one, where it is, and where it came from.
#[derive(Debug, Clone)]
pub struct FoundModel {
    pub model: Model,
    pub path: PathBuf,
    pub source: Source,
}

/// Candidate locations for one model, most specific first.
fn candidates(model: Model) -> Vec<(PathBuf, Source)> {
    let file = model.file_name();
    let mut out: Vec<(PathBuf, Source)> = Vec::new();

    if let Ok(env_path) = std::env::var("TEXTLAYR_MODEL") {
        out.push((PathBuf::from(env_path), Source::Env));
    }

    // Downloaded by the app.
    if let Some(dir) = paths::models_dir() {
        out.push((dir.join(file), Source::Downloaded));
    }

    // Bundled with the app (per-OS layout), then a source checkout.
    for (root, source) in crate::platform::bundle_roots() {
        out.push((root.join("models").join(file), source));
    }
    for (root, source) in crate::platform::dev_roots() {
        out.push((root.join("models").join(file), source));
    }

    out
}

/// Locate one specific model, if present.
pub fn locate(model: Model) -> Option<FoundModel> {
    candidates(model)
        .into_iter()
        .find(|(path, _)| path.is_file())
        .map(|(path, source)| FoundModel {
            model,
            path,
            source,
        })
}

/// Locate the model to use: `preferred` if installed, otherwise whichever
/// model is available (larger/more accurate first). `None` when no model is
/// installed at all.
pub fn resolve(preferred: Model) -> Option<FoundModel> {
    if let Some(found) = locate(preferred) {
        return Some(found);
    }
    // Prefer the larger model as the fallback: better accuracy beats speed when
    // the user's preference isn't actually on disk.
    let mut fallbacks: Vec<Model> = Model::ALL.into_iter().filter(|m| *m != preferred).collect();
    fallbacks.sort_by_key(|m| std::cmp::Reverse(m.approx_bytes()));
    fallbacks.into_iter().find_map(locate)
}

/// Delete a model that TextLayr downloaded.
///
/// Only touches TextLayr's own copy in the app data directory — a bundled, env-var
/// or dev-checkout model belongs to someone else and is never removed here.
pub fn remove_downloaded(found: &FoundModel) -> Result<()> {
    if found.source != Source::Downloaded {
        bail!(
            "not removing the {} model at {} (TextLayr only deletes models it downloaded)",
            found.source.label(),
            found.path.display()
        );
    }
    std::fs::remove_file(&found.path)
        .with_context(|| format!("failed to remove {}", found.path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for m in Model::ALL {
            assert_eq!(Model::from_id(m.id()), Some(m));
        }
        assert_eq!(Model::from_id("nope"), None);
    }

    #[test]
    fn each_model_has_a_distinct_file_and_hash() {
        let files: std::collections::HashSet<_> = Model::ALL.iter().map(|m| m.file_name()).collect();
        let hashes: std::collections::HashSet<_> = Model::ALL.iter().map(|m| m.sha256()).collect();
        assert_eq!(files.len(), Model::ALL.len());
        assert_eq!(hashes.len(), Model::ALL.len());
    }

    #[test]
    fn download_urls_are_pinned_and_named() {
        for m in Model::ALL {
            let url = m.download_url();
            assert!(url.contains(m.file_name()), "{url} should name {}", m.file_name());
            assert!(url.contains("/resolve/"), "{url} should be revision-pinned");
        }
    }
}
