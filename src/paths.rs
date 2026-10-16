//! Platform directories TextLayr owns for files it downloads at runtime.
//!
//! Kept separate from the source checkout so a downloaded binary or model is
//! visible and removable without touching the repo or the `.app` bundle.
//!
//! - macOS: `~/Library/Application Support/textlayr/`
//! - Linux: `~/.local/share/textlayr/`

use std::path::PathBuf;

/// Per-user data directory for TextLayr (`None` if the platform has none).
pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("textlayr"))
}

/// Where downloaded `ffmpeg`/`ffprobe` live.
pub fn bin_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("bin"))
}

/// Where downloaded whisper models live.
pub fn models_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("models"))
}
