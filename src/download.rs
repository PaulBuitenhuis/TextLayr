//! First-run downloads: fetch a file (or a binary inside a zip) to the app's
//! data directory, verifying a pinned SHA-256 before it is trusted.
//!
//! Transport is the system `curl` (always present on macOS) rather than an
//! HTTP crate: it keeps the dependency tree — and the binary — small. We read
//! curl's stdout ourselves so we can report byte-level progress and honour a
//! cancel flag without a UI-thread round-trip.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

const CURL: &str = "/usr/bin/curl";
const DITTO: &str = "/usr/bin/ditto";
const XATTR: &str = "/usr/bin/xattr";

/// Progress callback: `(bytes_downloaded, expected_total)`.
pub type ByteSink<'a> = dyn FnMut(u64, Option<u64>) + 'a;

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
    Ok(format!("{:x}", hasher.finalize()))
}

/// Ask the server for the total size (best effort — some CDNs omit it).
pub fn content_length(url: &str) -> Option<u64> {
    let out = Command::new(CURL)
        .args(["-sIL", "--max-time", "30", url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut len = None;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(n) = value.trim().parse::<u64>() {
                    len = Some(n);
                }
            }
        }
    }
    len
}

/// Stream `url` into `dest` (a temp path chosen by the caller), reporting bytes
/// as they arrive. Returns an error and removes `dest` if cancelled or if curl
/// fails, so a torn download is never mistaken for a complete one.
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

    let mut child = Command::new(CURL)
        // -sS: quiet but still show errors; -L follow redirects; --fail: HTTP
        // errors become a non-zero exit rather than an HTML error page.
        .args(["-sSL", "--fail", "--location", "--retry", "3"])
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {CURL}"))?;

    let mut stdout = child.stdout.take().expect("piped stdout");
    // Drain stderr on its own thread so a chatty curl can't fill the pipe and
    // deadlock us while we're blocked reading stdout.
    let stderr = child.stderr.take().expect("piped stderr");
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        s
    });

    let mut file = File::create(dest).with_context(|| format!("failed to create {}", dest.display()))?;
    let mut buf = [0u8; 128 * 1024];
    let mut got = 0u64;

    let outcome: Result<()> = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err(anyhow::anyhow!("cancelled"));
        }
        match stdout.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = file.write_all(&buf[..n]) {
                    let _ = child.kill();
                    break Err(e.into());
                }
                got += n as u64;
                on_progress(got, expected);
            }
            Err(e) => {
                let _ = child.kill();
                break Err(e.into());
            }
        }
    };

    let status = child.wait();
    let stderr_text = err_thread.join().unwrap_or_default();
    drop(file);

    let cleanup = |dest: &Path| {
        let _ = std::fs::remove_file(dest);
    };

    outcome?;
    match status {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => {
            cleanup(dest);
            let detail = stderr_text.trim();
            bail!(
                "download failed ({st}) for {url}{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                }
            );
        }
        Err(e) => {
            cleanup(dest);
            Err(e).context("failed to wait for curl")
        }
    }
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
    let total = expected_len.or_else(|| content_length(url));
    download_to(url, &tmp, total, cancel, on_progress)?;

    let got = sha256_file(&tmp)?;
    if !got.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&tmp);
        bail!("checksum mismatch for {url}: expected {expected_sha256}, got {got}");
    }
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("failed to place {}", dest.display()))?;
    Ok(())
}

/// Download a `.zip` and extract the binary named `binary_name` from it,
/// verifying its SHA-256 before placing it at `dest` (made executable).
pub fn fetch_zip_binary_verified(
    url: &str,
    binary_name: &str,
    dest: &Path,
    expected_sha256: &str,
    cancel: &AtomicBool,
    on_progress: &mut ByteSink<'_>,
) -> Result<()> {
    let work = tempfile::tempdir().context("temp dir for download")?;
    let zip = work.path().join("archive.zip");
    let total = content_length(url);
    download_to(url, &zip, total, cancel, on_progress)?;

    let out = work.path().join("extracted");
    std::fs::create_dir_all(&out)?;
    let status = Command::new(DITTO)
        .arg("-x")
        .arg("-k")
        .arg(&zip)
        .arg(&out)
        .status()
        .with_context(|| format!("failed to run {DITTO}"))?;
    if !status.success() {
        bail!("failed to extract the archive from {url}");
    }

    let found = find_file(&out, binary_name)
        .with_context(|| format!("{binary_name} not found inside the archive from {url}"))?;
    let got = sha256_file(&found)?;
    if !got.eq_ignore_ascii_case(expected_sha256) {
        bail!("checksum mismatch for {binary_name} from {url}: expected {expected_sha256}, got {got}");
    }

    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::copy(&found, dest).with_context(|| format!("failed to write {}", dest.display()))?;
    make_executable(dest)?;
    strip_quarantine(dest);
    Ok(())
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
    Ok(())
}

/// Best-effort removal of the macOS quarantine flag. curl usually doesn't set
/// it, but removing it unconditionally is harmless and avoids a "cannot be
/// opened" surprise if some layer did.
fn strip_quarantine(path: &Path) {
    let _ = Command::new(XATTR)
        .arg("-dr")
        .arg("com.apple.quarantine")
        .arg(path)
        .status();
}
