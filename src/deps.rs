//! Startup dependency check (Milestone 4).
//!
//! ffmpeg/ffprobe, the whisper model, and the bundled fonts are all required
//! before any work can start. [`Check::run`] locates them without side effects
//! and turns each miss into an actionable message the GUI banner and the CLI
//! can both show — instead of the user discovering the problem mid-export.

use crate::config;
use crate::model;
use crate::probe::{self, Fftools};
use std::path::PathBuf;

/// What the environment provides for a run. Each field is `None` when the
/// corresponding dependency could not be found.
#[derive(Debug, Clone, Default)]
pub struct Check {
    pub fftools: Option<Fftools>,
    pub model: Option<PathBuf>,
    pub fonts: Option<PathBuf>,
}

/// One missing requirement, phrased for a user.
#[derive(Debug, Clone)]
pub struct Problem {
    /// Short label, e.g. "ffmpeg/ffprobe".
    pub what: String,
    /// What to do about it.
    pub how: String,
}

impl Check {
    /// Locate every dependency. Never fails: misses are recorded as `None` so
    /// the caller can report them all at once.
    pub fn run() -> Check {
        Check {
            fftools: probe::find_ffmpeg().ok(),
            model: model::find_model().ok(),
            fonts: config::find_fonts_dir().ok(),
        }
    }

    /// True when nothing is missing.
    pub fn ok(&self) -> bool {
        self.fftools.is_some() && self.model.is_some() && self.fonts.is_some()
    }

    /// Actionable descriptions of what is missing (empty when [`Check::ok`]).
    pub fn problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        if self.fftools.is_none() {
            out.push(Problem {
                what: "ffmpeg / ffprobe".to_string(),
                how: format!(
                    "install them on your PATH (e.g. `brew install ffmpeg`), or place the \
                     binaries in {} (the packaged .app bundles them).",
                    vendor_dir()
                ),
            });
        }
        if self.model.is_none() {
            out.push(Problem {
                what: "whisper model".to_string(),
                how: "run scripts/fetch_model.sh, place ggml-small.en.bin in models/, \
                      or set $TEXXY_MODEL to its path."
                    .to_string(),
            });
        }
        if self.fonts.is_none() {
            out.push(Problem {
                what: "caption fonts".to_string(),
                how: "expected assets/fonts/*.ttf in the repo; set $TEXXY_FONTS to the \
                      directory that holds them."
                    .to_string(),
            });
        }
        out
    }

    /// A single multi-line message for the CLI, or `None` when all is well.
    pub fn error_message(&self) -> Option<String> {
        let problems = self.problems();
        if problems.is_empty() {
            return None;
        }
        let mut s = String::from("cannot start — missing dependencies:");
        for p in problems {
            s.push_str(&format!("\n  • {}: {}", p.what, p.how));
        }
        Some(s)
    }

    /// A compact one-line list of what is missing (for an in-app banner), or
    /// `None` when all is well.
    pub fn summary(&self) -> Option<String> {
        let problems = self.problems();
        if problems.is_empty() {
            return None;
        }
        Some(format!(
            "missing {}",
            problems
                .iter()
                .map(|p| p.what.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

/// Where a dev build looks for vendored binaries, per platform.
fn vendor_dir() -> &'static str {
    if cfg!(target_os = "macos") {
        "vendor/macos/"
    } else if cfg!(target_os = "linux") {
        "vendor/linux/"
    } else {
        "vendor/<platform>/"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_check_reports_every_dependency() {
        let check = Check::default();
        assert!(!check.ok());
        let problems = check.problems();
        assert_eq!(problems.len(), 3);
        assert!(check.error_message().unwrap().contains("ffmpeg"));
    }

    #[test]
    fn missing_only_model_reports_one_problem() {
        let check = Check {
            fftools: Some(Fftools {
                ffmpeg: PathBuf::from("/bin/true"),
                ffprobe: PathBuf::from("/bin/true"),
            }),
            model: None,
            fonts: Some(PathBuf::from("/fonts")),
        };
        assert!(!check.ok());
        let problems = check.problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].what, "whisper model");
    }
}
