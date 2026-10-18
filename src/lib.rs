//! TextLayr — offline karaoke caption burner for short-form video.
//!
//! The library holds the whole pipeline (probe → transcribe → chunk → render
//! → export), the shared [`pipeline`] entry points, and the egui front-end
//! ([`ui`]). The binary in `main.rs` dispatches between the headless CLI and
//! the GUI.

pub mod app;
pub mod audio;
pub mod chunk;
pub mod config;
pub mod deps;
pub mod download;
pub mod export;
pub mod install;
pub mod model;
pub mod paths;
pub mod pipeline;
pub mod platform;
pub mod probe;
pub mod render;
pub mod source;
pub mod transcribe;
pub mod ui;
pub mod update;

/// One-time migration from the project's former name (`texxy`).
///
/// Renames any existing per-user data/config/cache directory from `texxy` to
/// `textlayr` so an upgrade keeps its downloaded ffmpeg/model and preferences.
/// Best-effort: a failure (e.g. the target already exists) is reported and
/// otherwise ignored. Safe to call on every launch — it no-ops once migrated.
pub fn migrate_legacy_dirs() {
    for parent in [dirs::data_dir(), dirs::config_dir(), dirs::cache_dir()]
        .into_iter()
        .flatten()
    {
        let old = parent.join("texxy");
        let new = parent.join("textlayr");
        if !new.exists() && old.exists() {
            if let Err(e) = std::fs::rename(&old, &new) {
                eprintln!(
                    "warning: could not migrate {} to {}: {e}",
                    old.display(),
                    new.display()
                );
            }
        }
    }
}
