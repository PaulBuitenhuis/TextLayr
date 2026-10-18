//! TextLayr desktop binary.
//!
//! This is the GUI build. On Windows it hides the console window in release
//! builds; use the `textlayr-cli` binary when you want console output
//! (`--setup`, `--check-updates`, `--words`, `--headless`, …). On macOS and
//! Linux the binary serves both roles — `textlayr <video>` still runs the
//! headless pipeline — and `textlayr-cli` is an identical convenience alias.

// Hide the console window for the GUI build on Windows (keep it in debug builds
// so `eprintln!` diagnostics are visible while developing).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    textlayr::app::run();
}
