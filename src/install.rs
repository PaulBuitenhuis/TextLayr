//! First-run installation: fetch the pieces TextLayr needs but doesn't bundle.
//!
//! TextLayr ships only its own binary and fonts. On first run the setup screen can
//! download `ffmpeg`/`ffprobe` and the chosen whisper model into
//! `~/Library/Application Support/textlayr/`, where discovery looks for them
//! afterwards. Everything is checksum-pinned (see [`crate::download`]).
//!
//! `ffmpeg`/`ffprobe` are **not** redistributed by TextLayr: they are downloaded
//! straight from osxexperts.net by the user's machine. Those builds are GPL
//! (they include x264/x265); TextLayr merely invokes them as separate programs, so
//! this has no bearing on TextLayr's own MIT license.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::config;
use crate::download;
use crate::model::{self, Model};
use crate::paths;
use crate::probe;

// macOS arm64 static builds from osxexperts.net (the hashes are the ones the
// site publishes for the *unzipped* binaries). If osxexperts rolls a new
// version these URLs will 404 or the hashes will mismatch — a loud failure we
// fix by bumping the constants, never a silent bad download.
const FFMPEG_ZIP: &str = "https://www.osxexperts.net/ffmpeg9arm.zip";
const FFMPEG_SHA: &str = "591260c945d0eef150e3bf82b0ef988bd36a9cecc18ff05d6679617159f0a95e";
const FFPROBE_ZIP: &str = "https://www.osxexperts.net/ffprobe9arm.zip";
const FFPROBE_SHA: &str = "e11c17e8200b3ee4c4c186d245e2b4053f01d56957336c1817fca0b997469106";

/// One step of the install, for progress display.
#[derive(Debug, Clone)]
pub enum InstallProgress {
    /// Beginning the item at `index` of `total`.
    Begin {
        index: usize,
        total: usize,
        label: String,
    },
    /// Bytes received for the current item.
    Bytes {
        index: usize,
        total: usize,
        label: String,
        got: u64,
        expected: Option<u64>,
    },
    /// Item finished successfully.
    Installed {
        index: usize,
        total: usize,
        label: String,
    },
    /// Nothing was missing; instant success.
    NothingToDo,
}

/// Progress sink for [`install`].
pub type InstallSink = Box<dyn FnMut(InstallProgress) + Send>;

/// What a single job does.
enum Action {
    /// Download a zip and extract the named binary to `dest`.
    Binary {
        url: &'static str,
        sha: &'static str,
        bin: &'static str,
        dest: PathBuf,
    },
    /// Download a whisper model into the models directory.
    Model(Model),
}

struct Job {
    label: String,
    action: Action,
}

/// The jobs needed to make `preferred` runnable: ffmpeg/ffprobe if no tools are
/// found, and the model if the preferred one isn't installed.
fn required_jobs(preferred: Model) -> Result<Vec<Job>> {
    let mut jobs: Vec<Job> = Vec::new();

    if probe::find_ffmpeg().is_err() {
        let bin = paths::bin_dir().context("no application data directory for downloads")?;
        jobs.push(Job {
            label: "ffmpeg".to_string(),
            action: Action::Binary {
                url: FFMPEG_ZIP,
                sha: FFMPEG_SHA,
                bin: "ffmpeg",
                dest: bin.join("ffmpeg"),
            },
        });
        jobs.push(Job {
            label: "ffprobe".to_string(),
            action: Action::Binary {
                url: FFPROBE_ZIP,
                sha: FFPROBE_SHA,
                bin: "ffprobe",
                dest: bin.join("ffprobe"),
            },
        });
    }

    if model::locate(preferred).is_none() {
        jobs.push(Job {
            label: preferred.id().to_string(),
            action: Action::Model(preferred),
        });
    }

    Ok(jobs)
}

/// Run every required job, reporting progress through `sink`. Cancellable via
/// `cancel`.
pub fn install(preferred: Model, mut sink: InstallSink, cancel: Arc<AtomicBool>) -> Result<()> {
    let jobs = required_jobs(preferred)?;
    if jobs.is_empty() {
        sink(InstallProgress::NothingToDo);
        return Ok(());
    }
    let total = jobs.len();

    for (index, job) in jobs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("install cancelled");
        }
        sink(InstallProgress::Begin {
            index,
            total,
            label: job.label.clone(),
        });

        match &job.action {
            Action::Binary {
                url,
                sha,
                bin,
                dest,
            } => {
                let mut on = |got, expected| {
                    sink(InstallProgress::Bytes {
                        index,
                        total,
                        label: job.label.clone(),
                        got,
                        expected,
                    })
                };
                download::fetch_zip_binary_verified(url, bin, dest, sha, &cancel, &mut on)
                    .with_context(|| {
                        format!(
                            "couldn't download {} — you can install it yourself instead \
                             (e.g. `brew install ffmpeg`) and then click Re-check",
                            job.label
                        )
                    })?;
            }
            Action::Model(m) => {
                let dir = paths::models_dir().context("no application data directory")?;
                let dest = dir.join(m.file_name());
                let mut on = |got, expected| {
                    sink(InstallProgress::Bytes {
                        index,
                        total,
                        label: job.label.clone(),
                        got,
                        expected,
                    })
                };
                download::fetch_verified(
                    m.download_url(),
                    &dest,
                    m.sha256(),
                    Some(m.approx_bytes()),
                    &cancel,
                    &mut on,
                )
                .with_context(|| format!("failed to install the {} model", m.id()))?;
            }
        }

        sink(InstallProgress::Installed {
            index,
            total,
            label: job.label.clone(),
        });
    }

    // Make sure the fonts the app renders with are present; they're bundled and
    // can't be installed.
    if config::find_fonts_dir().is_err() {
        bail!("caption fonts are missing — reinstall TextLayr");
    }

    Ok(())
}

/// Total bytes of everything TextLayr downloaded (ffmpeg/ffprobe + models).
pub fn downloaded_size() -> u64 {
    [paths::bin_dir(), paths::models_dir()]
        .into_iter()
        .flatten()
        .map(|dir| dir_size(&dir))
        .sum()
}

fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Remove downloaded `ffmpeg`/`ffprobe` and models. Preferences, the install
/// receipt and the transcript cache are left alone, and a dev-checkout or
/// `$TEXTLAYR_MODEL` model is never in these directories.
pub fn remove_downloaded() -> Result<()> {
    for dir in [paths::bin_dir(), paths::models_dir()]
        .into_iter()
        .flatten()
    {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_jobs_is_total_for_every_model() {
        // We can't guarantee what's on disk in a dev checkout, so just assert
        // the planner returns without panicking for each model.
        let _ = required_jobs(Model::BaseEn);
        let _ = required_jobs(Model::SmallEn);
    }
}
