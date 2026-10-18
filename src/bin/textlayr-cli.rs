//! Console entry point.
//!
//! Identical behaviour to the `textlayr` binary, but this one always keeps the
//! console on Windows, so the output of `--setup`, `--check-updates`, `--words`
//! and `--headless` is visible. On macOS and Linux it is a convenience alias
//! (the main binary already behaves the same there).

fn main() {
    textlayr::app::run();
}
