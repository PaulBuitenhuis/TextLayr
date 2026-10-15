//! texxy — offline karaoke caption burner for short-form video.
//!
//! The library holds the whole pipeline (probe → transcribe → chunk → render
//! → export), the shared [`pipeline`] entry points, and the egui front-end
//! ([`ui`]). The binary in `main.rs` dispatches between the headless CLI and
//! the GUI.

pub mod audio;
pub mod chunk;
pub mod config;
pub mod deps;
pub mod export;
pub mod model;
pub mod pipeline;
pub mod probe;
pub mod render;
pub mod transcribe;
pub mod ui;
