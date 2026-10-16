//! Where a runtime dependency was found.
//!
//! The About window shows these labels so it's obvious whether texxy is using
//! something bundled with the app, something it downloaded on first run, or
//! something the user's system happens to provide. Without this, a dev build
//! that quietly fell back to `$PATH` looks identical to a self-contained one.

/// Provenance of a located dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Shipped inside `texxy.app/Contents/Resources`.
    Bundled,
    /// Downloaded by texxy into the platform data directory.
    Downloaded,
    /// Found on the user's `PATH` or provided by the system.
    System,
    /// Found in the source checkout (`vendor/`, `models/`, `assets/`) while
    /// developing.
    Dev,
    /// Pointed at explicitly by an environment variable.
    Env,
}

impl Source {
    /// A short, user-facing description for the About window.
    pub fn label(self) -> &'static str {
        match self {
            Source::Bundled => "bundled with the app",
            Source::Downloaded => "downloaded",
            Source::System => "system (PATH)",
            Source::Dev => "dev checkout",
            Source::Env => "env var",
        }
    }
}
