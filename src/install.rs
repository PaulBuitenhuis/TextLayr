//! First-run installation: fetch the pieces TextLayr needs but doesn't bundle.
//!
//! TextLayr ships only its own binary and fonts. On first run the setup screen can
//! download `ffmpeg`/`ffprobe` and the chosen whisper model into the platform
//! data directory, where discovery looks for them afterwards. Everything is
//! checksum-pinned (see [`crate::download`]).
//!
//! `ffmpeg`/`ffprobe` are **not** redistributed by TextLayr: they are downloaded
//! straight from a public build host by the user's machine, into the platform
//! data directory. Those builds are GPL (they include x264/x265); TextLayr merely
//! invokes them as separate programs, so this has no bearing on TextLayr's own MIT
//! license. Which host and archive depends on the platform (see [`tools_archives`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::config;
use crate::download;
use crate::model::{self, Model};
use crate::paths;
use crate::platform::{self, Os};
use crate::probe;

// ── macOS (Apple Silicon): static arm64 builds from osxexperts.net ──────────
// The site publishes the SHA-256 of the *unzipped* binaries, so these are pinned
// as per-binary checksums. If osxexperts rolls a new version these URLs will 404
// or the hashes will mismatch — a loud failure we fix by bumping the constants,
// never a silent bad download.
const MACOS_FFMPEG_ZIP: &str = "https://www.osxexperts.net/ffmpeg9arm.zip";
const MACOS_FFMPEG_SHA: &str = "591260c945d0eef150e3bf82b0ef988bd36a9cecc18ff05d6679617159f0a95e";
const MACOS_FFPROBE_ZIP: &str = "https://www.osxexperts.net/ffprobe9arm.zip";
const MACOS_FFPROBE_SHA: &str = "e11c17e8200b3ee4c4c186d245e2b4053f01d56957336c1817fca0b997469106";

// ── Linux: a BtbN/FFmpeg-Builds static GPL build ────────────────────────────
// Pinned to a *dated* autobuild tag so both the URL and GitHub's published
// archive digest are immutable. To bump ffmpeg: pick a newer dated tag and update
// the URL, the tag embedded in the URL, and the SHA-256 (from GitHub's release
// API `digest` field for that asset).
const BTBN_LINUX_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-16-13-04/ffmpeg-N-127579-g5cb9fcd211-linux64-gpl.tar.xz";
const BTBN_LINUX_SHA: &str = "3d994f31aa9ee78ef56eab530a86d50d8307200cb9d1788be36c1ea9410e605c";

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

/// An upstream archive of external tools plus the binaries it provides.
struct ToolsArchive {
    url: &'static str,
    checksum: download::Checksum<'static>,
    format: download::ArchiveFormat,
    /// Binary base names inside the archive (without any `.exe`).
    binaries: &'static [&'static str],
}

/// The archive(s) that provide `ffmpeg`/`ffprobe`, for the current platform.
///
/// macOS ships the two tools as separate zips; Linux gets a single `.tar.xz`
/// containing both. Windows packaging is part of the separate Windows item, so
/// it deliberately has no entry yet.
fn tools_archives() -> Vec<ToolsArchive> {
    match platform::os() {
        Os::Macos => vec![
            ToolsArchive {
                url: MACOS_FFMPEG_ZIP,
                checksum: download::Checksum::Binary(MACOS_FFMPEG_SHA),
                format: download::ArchiveFormat::Zip,
                binaries: &["ffmpeg"],
            },
            ToolsArchive {
                url: MACOS_FFPROBE_ZIP,
                checksum: download::Checksum::Binary(MACOS_FFPROBE_SHA),
                format: download::ArchiveFormat::Zip,
                binaries: &["ffprobe"],
            },
        ],
        Os::Linux => vec![ToolsArchive {
            url: BTBN_LINUX_URL,
            checksum: download::Checksum::Archive(BTBN_LINUX_SHA),
            format: download::ArchiveFormat::TarXz,
            binaries: &["ffmpeg", "ffprobe"],
        }],
        Os::Windows => Vec::new(),
    }
}

/// What a single job does.
enum Action {
    /// Download an archive and install its binaries into the app's bin dir.
    Tools(ToolsArchive),
    /// Download a whisper model into the models directory.
    Model(Model),
}

struct Job {
    label: String,
    action: Action,
}

/// The jobs needed to make `preferred` runnable: the platform's ffmpeg/ffprobe
/// archive(s) if no tools are found, and the model if the preferred one isn't
/// installed.
fn required_jobs(preferred: Model) -> Result<Vec<Job>> {
    let mut jobs: Vec<Job> = Vec::new();

    if probe::find_ffmpeg().is_err() {
        // Fail fast if there's nowhere to put the tools.
        paths::bin_dir().context("no application data directory for downloads")?;
        for archive in tools_archives() {
            jobs.push(Job {
                label: archive.binaries.join(" + "),
                action: Action::Tools(archive),
            });
        }
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
            Action::Tools(archive) => {
                let bin_dir =
                    paths::bin_dir().context("no application data directory for downloads")?;
                let mut on = |got, expected| {
                    sink(InstallProgress::Bytes {
                        index,
                        total,
                        label: job.label.clone(),
                        got,
                        expected,
                    })
                };
                download::fetch_archive_binaries_verified(
                    archive.url,
                    archive.format,
                    archive.binaries,
                    &bin_dir,
                    archive.checksum,
                    &cancel,
                    &mut on,
                )
                .with_context(|| {
                    format!(
                        "couldn't download {} — you can install it yourself instead \
                         (e.g. `brew install ffmpeg` or your package manager) and then \
                         click Re-check",
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
    fn required_jobs_is_total_and_plans_a_model_when_it_is_missing() {
        // We can't guarantee what's on disk in a dev checkout, so the planner
        // must be total; when a model is absent its job must be planned.
        for model in Model::ALL {
            let jobs = required_jobs(model)
                .unwrap_or_else(|e| panic!("planning {} failed: {e:#}", model.id()));
            if crate::model::locate(model).is_none() {
                assert!(
                    jobs.iter()
                        .any(|j| matches!(&j.action, Action::Model(m) if *m == model)),
                    "{} is not installed but no download was planned",
                    model.id()
                );
            }
            assert!(
                jobs.iter().all(|j| !j.label.trim().is_empty()),
                "every job needs a progress label"
            );
        }
    }

    #[test]
    fn tools_archives_are_pinned_and_well_formed() {
        let archives = tools_archives();
        assert!(!archives.is_empty(), "the host platform needs tools");
        for archive in &archives {
            assert!(archive.url.starts_with("https://"), "{}", archive.url);
            assert!(!archive.binaries.is_empty());
            assert!(
                archive.binaries.iter().all(|b| !b.contains('.')),
                "binary names are base names; the extension is platform-specific"
            );
            let sha = match archive.checksum {
                download::Checksum::Archive(s) | download::Checksum::Binary(s) => s,
            };
            assert_eq!(sha.len(), 64, "{} needs a full SHA-256", archive.url);
            assert!(
                sha.bytes().all(|b| b.is_ascii_hexdigit()),
                "{} has a non-hex checksum",
                archive.url
            );
        }
    }

    /// A Linux build must never fetch the macOS (arm64) binaries, and vice versa.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uses_the_btbn_tarball_only() {
        let archives = tools_archives();
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].url, BTBN_LINUX_URL);
        assert_eq!(archives[0].format, download::ArchiveFormat::TarXz);
        assert!(!archives[0].url.contains("osxexperts"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_the_osxexperts_zips_only() {
        let archives = tools_archives();
        assert_eq!(archives.len(), 2);
        assert!(archives.iter().all(|a| a.url.contains("osxexperts.net")));
        assert!(archives
            .iter()
            .all(|a| a.format == download::ArchiveFormat::Zip));
    }
}
