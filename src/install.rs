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

// ── Linux + Windows: BtbN/FFmpeg-Builds static GPL builds ───────────────────
// Pinned to a *dated* autobuild tag so both the URL and GitHub's published
// archive digest are immutable. To bump ffmpeg: pick a newer dated tag and update
// the URL, the tag embedded in the URL, and the SHA-256 (from GitHub's release
// API `digest` field for that asset).
const BTBN_LINUX_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-16-13-04/ffmpeg-N-127579-g5cb9fcd211-linux64-gpl.tar.xz";
const BTBN_LINUX_SHA: &str = "3d994f31aa9ee78ef56eab530a86d50d8307200cb9d1788be36c1ea9410e605c";
const BTBN_WINDOWS_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-16-13-04/ffmpeg-N-127579-g5cb9fcd211-win64-gpl.zip";
const BTBN_WINDOWS_SHA: &str = "b7c94a054ae4c1cce0e11ae18979d932e6f129ba6f5253b328227b31f1bf2bbf";

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
/// macOS ships the two tools as separate zips; Linux and Windows get a single
/// archive (`.tar.xz` / `.zip`) containing both.
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
        Os::Windows => vec![ToolsArchive {
            url: BTBN_WINDOWS_URL,
            checksum: download::Checksum::Archive(BTBN_WINDOWS_SHA),
            format: download::ArchiveFormat::Zip,
            binaries: &["ffmpeg", "ffprobe"],
        }],
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
                         (`brew install ffmpeg` on macOS, your package manager on Linux, \
                         or winget/scoop on Windows) and then click Re-check",
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

    #[test]
    fn every_platform_has_a_tools_archive() {
        // The recipe table is total: whatever the OS, ffmpeg/ffprobe are covered.
        let archives = tools_archives();
        assert!(!archives.is_empty());
        let names: Vec<&str> = archives
            .iter()
            .flat_map(|a| a.binaries.iter().copied())
            .collect();
        assert!(names.contains(&"ffmpeg"), "{names:?}");
        assert!(names.contains(&"ffprobe"), "{names:?}");
    }

    #[test]
    fn pinned_urls_are_immutable_where_required() {
        // BtbN URLs must carry a dated tag, never the rolling `latest`.
        for a in tools_archives() {
            if a.url.contains("BtbN") {
                assert!(a.url.contains("/download/autobuild-"), "{}", a.url);
                assert!(!a.url.contains("/download/latest/"), "{}", a.url);
            }
        }
    }

    /// Network smoke test for the macOS path: download the real osxexperts zip
    /// through the pure-Rust transport, extract it with the `zip` crate, and
    /// confirm the binary still matches the pinned hash (catches upstream
    /// re-rolls and zip-crate extraction issues). Ignored by default; run with
    /// `cargo test -- --ignored`.
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "network"]
    fn macos_ffmpeg_zip_extracts_and_matches_the_pin() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let mut sink = |_got: u64, _total: Option<u64>| {};
        download::fetch_archive_binaries_verified(
            MACOS_FFMPEG_ZIP,
            download::ArchiveFormat::Zip,
            &["ffmpeg"],
            dir.path(),
            download::Checksum::Binary(MACOS_FFMPEG_SHA),
            &cancel,
            &mut sink,
        )
        .unwrap();
        assert!(dir.path().join("ffmpeg").is_file());
    }
}
