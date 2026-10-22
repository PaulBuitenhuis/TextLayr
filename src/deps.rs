//! Startup dependency check.
//!
//! ffmpeg/ffprobe, a whisper model and the bundled fonts are all required
//! before any work can start. [`Check::run`] locates them without side effects
//! and records where each came from, so the GUI can (a) offer to install what's
//! missing and (b) show honest provenance in the About window, and the CLI can
//! print one actionable error instead of failing mid-export.

use std::path::PathBuf;

use crate::config;
use crate::model::{self, FoundModel, Model};
use crate::probe::{self, Fftools};
use crate::source::Source;

/// What the environment provides for a run. Each field is `None` when the
/// corresponding dependency could not be found.
#[derive(Debug, Clone, Default)]
pub struct Check {
    pub fftools: Option<Fftools>,
    pub model: Option<FoundModel>,
    pub fonts: Option<(PathBuf, Source)>,
}

/// One missing requirement, phrased for a user.
#[derive(Debug, Clone)]
pub struct Problem {
    /// Short label, e.g. "ffmpeg/ffprobe".
    pub what: String,
    /// What to do about it.
    pub how: String,
    /// Whether the app can fetch this itself (Setup → Install).
    pub installable: bool,
}

impl Check {
    /// Locate every dependency for the given model preference. Never fails:
    /// misses are recorded as `None` so the caller can report them all at once.
    pub fn run(preferred: Model) -> Check {
        Check {
            fftools: probe::find_ffmpeg().ok(),
            model: model::resolve(preferred),
            fonts: config::find_fonts_dir().ok(),
        }
    }

    /// True when nothing is missing.
    pub fn ok(&self) -> bool {
        self.fftools.is_some() && self.model.is_some() && self.fonts.is_some()
    }

    /// Actionable descriptions of what is missing (empty when [`Check::ok`]).
    pub fn problems(&self) -> Vec<Problem> {
        self.problems_with(true)
    }

    /// [`Self::problems`], optionally ignoring a missing model. `--transcript`
    /// supplies its own word timings, so the model is not a requirement there.
    fn problems_with(&self, include_model: bool) -> Vec<Problem> {
        let mut out = Vec::new();
        if self.fftools.is_none() {
            out.push(Problem {
                what: "ffmpeg / ffprobe".to_string(),
                how: "Click Install to download them.".to_string(),
                installable: true,
            });
        }
        if include_model && self.model.is_none() {
            out.push(Problem {
                what: "whisper model".to_string(),
                how: "Click Install to download it.".to_string(),
                installable: true,
            });
        }
        if self.fonts.is_none() {
            out.push(Problem {
                what: "caption fonts".to_string(),
                how: "The bundled caption fonts are missing, which means the app is \
                      damaged. Reinstall TextLayr."
                    .to_string(),
                installable: false,
            });
        }
        out
    }

    /// True when at least one missing item can be fetched by the app.
    pub fn has_installable(&self) -> bool {
        self.problems().iter().any(|p| p.installable)
    }

    /// A single multi-line message for the CLI, or `None` when all is well.
    pub fn error_message(&self) -> Option<String> {
        self.error_message_impl(true)
    }

    /// [`Self::error_message`] ignoring a missing whisper model, for a run that
    /// supplies its own transcript (`--transcript`).
    pub fn error_message_sans_model(&self) -> Option<String> {
        self.error_message_impl(false)
    }

    fn error_message_impl(&self, include_model: bool) -> Option<String> {
        let problems = self.problems_with(include_model);
        if problems.is_empty() {
            return None;
        }
        let mut s = String::from("cannot start — missing dependencies:");
        for p in problems {
            // The GUI offers an Install button; the CLI has `--setup`.
            let hint = if p.installable {
                "run `textlayr --setup` to download it"
            } else {
                p.how.as_str()
            };
            s.push_str(&format!("\n  • {}: {}", p.what, hint));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_check_reports_every_dependency() {
        let check = Check::default();
        assert!(!check.ok());
        let problems = check.problems();
        assert_eq!(problems.len(), 3);
        let message = check.error_message().unwrap();
        assert!(message.contains("ffmpeg"));
        assert!(message.contains("--setup"));
        assert!(!message.contains("Click Install"));
        // ffmpeg and the model can be installed; fonts cannot.
        assert!(check.has_installable());
    }

    #[test]
    fn missing_only_model_reports_one_problem() {
        let check = Check {
            fftools: Some(Fftools {
                ffmpeg: PathBuf::from("/bin/true"),
                ffprobe: PathBuf::from("/bin/true"),
                source: Source::System,
            }),
            model: None,
            fonts: Some((PathBuf::from("/fonts"), Source::Bundled)),
        };
        assert!(!check.ok());
        let problems = check.problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].what, "whisper model");
        assert!(problems[0].installable);
    }

    #[test]
    fn a_transcript_run_does_not_need_a_model() {
        let only_model_missing = Check {
            fftools: Some(Fftools {
                ffmpeg: PathBuf::from("/bin/true"),
                ffprobe: PathBuf::from("/bin/true"),
                source: Source::System,
            }),
            model: None,
            fonts: Some((PathBuf::from("/fonts"), Source::Bundled)),
        };
        assert!(only_model_missing.error_message().is_some());
        assert_eq!(only_model_missing.error_message_sans_model(), None);

        // Everything missing: the transcript form still names the real
        // blockers (ffmpeg, fonts) but not the model.
        let empty = Check::default();
        let msg = empty.error_message_sans_model().unwrap();
        assert!(msg.contains("ffmpeg"), "{msg}");
        assert!(msg.contains("caption fonts"), "{msg}");
        assert!(!msg.contains("whisper model"), "{msg}");
    }
}
