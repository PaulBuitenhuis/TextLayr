//! First-run downloads: fetch a file (or a binary inside an archive) to the
//! app's data directory, verifying a pinned SHA-256 before it is trusted.
//!
//! Transport is a pure-Rust HTTP client (`ureq`, rustls) and extraction uses
//! pure-Rust decoders, so the same code serves macOS, Linux and Windows with no
//! dependency on system tools (`curl`, `ditto`). A `file://` URL is served
//! straight from the local filesystem, which the updater's local-testing flow
//! (`$TEXTLAYR_UPDATE_URL`) relies on.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::platform;

/// Progress callback: `(bytes_downloaded, expected_total)`.
pub type ByteSink<'a> = dyn FnMut(u64, Option<u64>) + 'a;

/// How many times to retry a failed HTTP request before giving up.
const HTTP_ATTEMPTS: usize = 3;

/// The archive container a downloadable binary lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// A `.zip` (the macOS ffmpeg builds).
    Zip,
    /// A `.tar.xz` (Linux static ffmpeg builds).
    TarXz,
    /// A `.tar.gz`.
    TarGz,
}

/// What a pinned SHA-256 refers to.
///
/// GitHub release digests cover the *archive*; the macOS `osxexperts.net` site
/// publishes the hash of the *unzipped* binary. Both are supported so each
/// source can be pinned the way it is actually published.
#[derive(Debug, Clone, Copy)]
pub enum Checksum<'a> {
    /// The downloaded archive file itself.
    Archive(&'a str),
    /// A single extracted binary (the archive must contain exactly one).
    Binary(&'a str),
}

/// SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    // sha2 0.11's digest `Output` no longer implements `LowerHex`, so format the
    // bytes ourselves (lowercase hex, matching the pinned checksums).
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Ask the server for the total size (best effort). A `file://` URL reports the
/// file's size. Some hosts (GitHub release assets, which redirect) don't answer
/// `HEAD` with a length — callers treat `None` as "unknown" and rely on the size
/// the actual download response reports ([`download_to`] does).
pub fn content_length(url: &str) -> Option<u64> {
    if let Some(path) = file_url_path(url) {
        return std::fs::metadata(path).ok().map(|m| m.len());
    }
    let resp = ureq::head(url).call().ok()?;
    resp.body().content_length()
}

/// Fetch a small resource into memory — an update manifest, a GitHub API
/// response, or a `SHA256SUMS` file. `accept`, when set, is sent as the
/// `Accept` header (the GitHub REST API wants `application/vnd.github+json`).
pub fn get_bytes(url: &str, cancel: &AtomicBool, accept: Option<&str>) -> Result<Vec<u8>> {
    if let Some(path) = file_url_path(url) {
        return std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()));
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }

    let mut attempt = 0usize;
    let resp = loop {
        let mut req = ureq::get(url);
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        match req.call() {
            Ok(resp) => break resp,
            Err(e) => {
                attempt += 1;
                if attempt >= HTTP_ATTEMPTS {
                    return Err(anyhow::anyhow!("could not fetch {url}: {e}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
            }
        }
    };

    let mut bytes = Vec::new();
    resp.into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .context("failed to read the response body")?;
    Ok(bytes)
}

/// Stream `url` into `dest`, reporting bytes as they arrive. Returns an error
/// and removes `dest` if cancelled or if the transfer fails, so a torn download
/// is never mistaken for a complete one.
pub fn download_to(
    url: &str,
    dest: &Path,
    expected: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &mut ByteSink<'_>,
) -> Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }

    // A local file (the updater's `file://` testing flow) needs no HTTP.
    if let Some(src) = file_url_path(url) {
        return copy_local(&src, dest, expected, cancel, on_progress);
    }

    // Retry transient transport failures (matching the old `curl --retry 3`).
    let mut attempt = 0usize;
    let resp = loop {
        match ureq::get(url).call() {
            Ok(resp) => break resp,
            Err(e) => {
                attempt += 1;
                if attempt >= HTTP_ATTEMPTS {
                    return Err(anyhow::anyhow!("download failed for {url}: {e}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
            }
        }
    };

    let total = expected.or_else(|| resp.body().content_length());
    let mut reader = resp.into_body().into_reader();
    copy_reader(&mut reader, dest, total, cancel, on_progress)
}

/// Pump a reader into `dest`, honouring cancel and reporting progress.
fn copy_reader(
    reader: &mut impl Read,
    dest: &Path,
    total: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &mut ByteSink<'_>,
) -> Result<()> {
    let mut file =
        File::create(dest).with_context(|| format!("failed to create {}", dest.display()))?;
    let mut buf = [0u8; 128 * 1024];
    let mut got = 0u64;

    let outcome: Result<()> = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err(anyhow::anyhow!("cancelled"));
        }
        match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = file.write_all(&buf[..n]) {
                    break Err(e.into());
                }
                got += n as u64;
                on_progress(got, total);
            }
            Err(e) => break Err(e.into()),
        }
    };

    drop(file);
    if outcome.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    outcome
}

/// Copy a local file into `dest` (the `file://` transport).
fn copy_local(
    src: &Path,
    dest: &Path,
    expected: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &mut ByteSink<'_>,
) -> Result<()> {
    let mut reader =
        File::open(src).with_context(|| format!("failed to open {}", src.display()))?;
    let total = expected.or_else(|| std::fs::metadata(src).ok().map(|m| m.len()));
    copy_reader(&mut reader, dest, total, cancel, on_progress)
}

/// Download `url` and require it to hash to `expected_sha256` before placing it
/// at `dest` (atomic rename; the `.part` file is cleaned up on any failure).
pub fn fetch_verified(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    expected_len: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &mut ByteSink<'_>,
) -> Result<()> {
    let tmp = part_path(dest);
    download_to(url, &tmp, expected_len, cancel, on_progress)?;

    let got = sha256_file(&tmp)?;
    if !got.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&tmp);
        bail!("checksum mismatch for {url}: expected {expected_sha256}, got {got}");
    }
    std::fs::rename(&tmp, dest).with_context(|| format!("failed to place {}", dest.display()))?;
    Ok(())
}

/// Download an archive and extract `binaries` (by name, searched recursively)
/// into `dest_dir`, verifying `checksum` before anything is trusted. Each
/// extracted binary is made executable and (on macOS) de-quarantined.
///
/// Unlike the old `curl`/`ditto` path this handles both `.zip` and `.tar.xz`,
/// which is what lets one code path serve every platform.
pub fn fetch_archive_binaries_verified(
    url: &str,
    format: ArchiveFormat,
    binaries: &[&str],
    dest_dir: &Path,
    checksum: Checksum<'_>,
    cancel: &AtomicBool,
    on_progress: &mut ByteSink<'_>,
) -> Result<()> {
    if let Checksum::Binary(_) = checksum {
        if binaries.len() != 1 {
            bail!("a per-binary checksum requires exactly one binary");
        }
    }

    let work = tempfile::tempdir().context("temp dir for download")?;
    let archive = work.path().join("archive");
    // `download_to` derives the total from the response itself.
    download_to(url, &archive, None, cancel, on_progress)?;

    if let Checksum::Archive(expected) = checksum {
        verify_sha(&archive, expected, url)?;
    }

    let out = work.path().join("extracted");
    std::fs::create_dir_all(&out).context("create extraction dir")?;
    extract(&archive, format, &out)
        .with_context(|| format!("failed to extract the archive from {url}"))?;

    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;

    for name in binaries {
        let wanted = platform::exe_name(name);
        let found = find_file(&out, &wanted)
            .with_context(|| format!("{wanted} not found inside the archive from {url}"))?;

        if let Checksum::Binary(expected) = checksum {
            verify_sha(&found, expected, url)?;
        }

        let dest = dest_dir.join(&wanted);
        std::fs::copy(&found, &dest)
            .with_context(|| format!("failed to write {}", dest.display()))?;
        make_executable(&dest)?;
        strip_quarantine(&dest);
    }
    Ok(())
}

/// Extract an archive into `out` using a pure-Rust decoder per format.
fn extract(archive: &Path, format: ArchiveFormat, out: &Path) -> Result<()> {
    let file =
        File::open(archive).with_context(|| format!("failed to open {}", archive.display()))?;
    let reader = BufReader::new(file);
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(reader).context("read zip archive")?;
            zip.extract(out).context("extract zip archive")?;
        }
        ArchiveFormat::TarXz => {
            let dec = xz2::read::XzDecoder::new(reader);
            tar::Archive::new(dec)
                .unpack(out)
                .context("extract tar.xz archive")?;
        }
        ArchiveFormat::TarGz => {
            let dec = flate2::read::GzDecoder::new(reader);
            tar::Archive::new(dec)
                .unpack(out)
                .context("extract tar.gz archive")?;
        }
    }
    Ok(())
}

/// Verify `path` hashes to `expected` (case-insensitive), naming `what` on error.
fn verify_sha(path: &Path, expected: &str, what: &str) -> Result<()> {
    let got = sha256_file(path)?;
    if !got.eq_ignore_ascii_case(expected) {
        bail!("checksum mismatch for {what}: expected {expected}, got {got}");
    }
    Ok(())
}

/// The local path behind a `file://` URL, or `None` for an http(s) URL.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file:///abs/path` -> `/abs/path`; on Windows `file:///C:/…` -> `C:/…`.
    if cfg!(windows) {
        Some(PathBuf::from(rest.trim_start_matches('/')))
    } else {
        Some(PathBuf::from(rest))
    }
}

/// The temp path used while a download is in flight.
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Find a file named `name` anywhere under `root`.
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Some(path);
            }
        }
    }
    None
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Best-effort removal of the macOS quarantine flag. The downloader doesn't set
/// it, but removing it unconditionally is harmless and avoids a "cannot be
/// opened" surprise if some layer did.
#[cfg(target_os = "macos")]
fn strip_quarantine(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/xattr")
        .arg("-dr")
        .arg("com.apple.quarantine")
        .arg(path)
        .status();
}

#[cfg(not(target_os = "macos"))]
fn strip_quarantine(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_map_to_local_paths() {
        assert!(file_url_path("https://example.com/x.zip").is_none());
        let p = file_url_path("file:///tmp/thing.zip").unwrap();
        assert!(p.is_absolute() || cfg!(windows));
    }

    #[test]
    fn copies_a_local_file_reporting_progress() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"hello world").unwrap();

        let cancel = AtomicBool::new(false);
        let mut seen = 0u64;
        let mut sink = |got: u64, _total: Option<u64>| seen = got;
        download_to(
            &format!("file://{}", src.display()),
            &dst,
            None,
            &cancel,
            &mut sink,
        )
        .unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"hello world");
        // Emits the full byte count, so a progress bar reaches 100%.
        assert_eq!(seen, 11);
    }

    #[test]
    fn cancellation_removes_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, vec![0u8; 4096]).unwrap();

        let cancel = AtomicBool::new(true);
        let mut sink = |_got: u64, _total: Option<u64>| {};
        let err = download_to(
            &format!("file://{}", src.display()),
            &dst,
            None,
            &cancel,
            &mut sink,
        )
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(!dst.exists());
    }

    #[test]
    fn a_tar_xz_round_trips_and_finds_a_member() {
        // Build a tiny .tar.xz containing an "ffmpeg" file, then exercise the
        // extraction path end to end (no network).
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("stage");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        // Extraction looks up the platform's executable name (`ffmpeg.exe` on
        // Windows), so the fixture must be named the same way.
        let ffmpeg = platform::exe_name("ffmpeg");
        std::fs::write(payload.join("bin").join(&ffmpeg), b"#!/bin/sh\n").unwrap();

        let tarball = dir.path().join("tools.tar.xz");
        {
            let out = File::create(&tarball).unwrap();
            let enc = xz2::write::XzEncoder::new(out, 6);
            let mut builder = tar::Builder::new(enc);
            builder.append_dir_all(".", &payload).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let dest = dir.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut sink = |_got: u64, _total: Option<u64>| {};
        // Archive hash is whatever we just wrote.
        let sha = sha256_file(&tarball).unwrap();
        fetch_archive_binaries_verified(
            &format!("file://{}", tarball.display()),
            ArchiveFormat::TarXz,
            &["ffmpeg"],
            &dest,
            Checksum::Archive(&sha),
            &cancel,
            &mut sink,
        )
        .unwrap();
        assert!(dest.join(platform::exe_name("ffmpeg")).is_file());
    }

    #[test]
    fn a_wrong_archive_checksum_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("tools.tar.xz");
        {
            let out = File::create(&tarball).unwrap();
            let enc = xz2::write::XzEncoder::new(out, 6);
            let mut builder = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "ffmpeg", &b"data"[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dest = dir.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut sink = |_got: u64, _total: Option<u64>| {};
        let err = fetch_archive_binaries_verified(
            &format!("file://{}", tarball.display()),
            ArchiveFormat::TarXz,
            &["ffmpeg"],
            &dest,
            Checksum::Archive(&"0".repeat(64)),
            &cancel,
            &mut sink,
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }
}
