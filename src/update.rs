//! In-app update check and self-install.
//!
//! TextLayr is distributed as `TextLayr-<version>.{dmg,zip}` on GitHub Releases.
//! The updater reads a small **manifest** (`latest.json`) that the release
//! workflow attaches to every release; the fixed URL
//! `releases/latest/download/latest.json` always resolves to the newest
//! published asset. It then decides whether that is newer than the running
//! build and — when the app is running from a writable `TextLayr.app` —
//! downloads, verifies and swaps the bundle in place via a small relaunch
//! helper.
//!
//! Trust model: unlike the first-run downloads in [`crate::install`], the hash
//! of a *future* release can't be pinned at compile time. The manifest names
//! each artifact with its SHA-256, served over TLS from GitHub, and the
//! extracted bundle is additionally checked for the expected
//! identifier/version and (on macOS) a valid codesign signature before anything
//! is replaced.
//!
//! Two optional overrides make the updater testable without publishing:
//! `$TEXTLAYR_UPDATE_URL` points at an alternate manifest (a URL, a `file://`
//! URL, or a local path — sibling files are resolved next to it), and
//! `$TEXTLAYR_UPDATE_FORCE` bypasses the "is it newer?" gate. Transport is the
//! shared pure-Rust downloader in [`crate::download`], so no system `curl` is
//! involved.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::download;
use crate::paths;
use crate::platform::{self, Os};

/// Default source of update information: a tiny manifest attached to the
/// newest GitHub release. The URL is version-independent — GitHub redirects
/// `latest/download/<asset>` to whatever is currently the latest release.
const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/PaulBuitenhuis/TextLayr/releases/latest/download/latest.json";
/// GitHub REST fallback for when the manifest asset doesn't exist (any release
/// cut before the manifest shipped).
const API_LATEST: &str = "https://api.github.com/repos/PaulBuitenhuis/TextLayr/releases/latest";
/// Human-facing releases page, used when the manifest gives no notes URL.
const RELEASES_URL: &str = "https://github.com/PaulBuitenhuis/TextLayr/releases/latest";

const DITTO: &str = "/usr/bin/ditto";
const XATTR: &str = "/usr/bin/xattr";
const CODESIGN: &str = "/usr/bin/codesign";
const OPEN: &str = "/usr/bin/open";

/// Bundle identifier a downloaded app must carry before it is trusted.
const BUNDLE_ID: &str = "com.paulbuitenhuis.textlayr";
/// The app bundle name inside the release archive.
const APP_NAME: &str = "TextLayr.app";

/// The version this build reports (`Cargo.toml`).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Whether to treat the fetched release as available regardless of version —
/// for local testing (`$TEXTLAYR_UPDATE_FORCE`).
pub fn forced() -> bool {
    env_flag("TEXTLAYR_UPDATE_FORCE")
}

fn skip_codesign() -> bool {
    env_flag("TEXTLAYR_UPDATE_SKIP_CODESIGN")
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

/// One downloadable file (the app archive or disk image).
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    /// Resolved download URL (sibling of the manifest).
    pub url: String,
    pub size: Option<u64>,
    /// Lowercase hex SHA-256, as published in the manifest.
    pub sha256: Option<String>,
}

/// How the platform's primary artifact is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// A `.zip` holding `TextLayr.app` — swapped in place on macOS.
    MacApp,
    /// A `.tar.*` holding the `textlayr` binary — handed over on Linux.
    UnixBinaryTar,
    /// An `.AppImage` — handed over on Linux.
    UnixAppImage,
}

/// The platform's primary installable artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub asset: Asset,
    pub kind: ArtifactKind,
}

/// The newest published release, resolved for the running platform.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// Version with any leading `v` removed, e.g. `0.3.0`.
    pub version: String,
    /// Link to the release notes.
    pub notes_url: String,
    /// What this platform installs from, when the manifest has an entry for it.
    pub primary: Option<Artifact>,
    /// A manual hand-off artifact (the macOS `.dmg`), when published.
    pub fallback: Option<Asset>,
}

// ── manifest (as written by .github/workflows/release.yml) ─────────────────

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes_url: Option<String>,
    /// Legacy top-level macOS app archive (manifests written before `platforms`).
    #[serde(default)]
    zip: Option<ManifestAsset>,
    /// Legacy top-level macOS disk image.
    #[serde(default)]
    dmg: Option<ManifestAsset>,
    /// Per-platform assets, keyed by `platform::slug()` (`macos-arm64`, …).
    #[serde(default)]
    platforms: std::collections::HashMap<String, PlatformAssets>,
}

/// The assets published for one platform.
#[derive(Debug, Clone, Default, Deserialize)]
struct PlatformAssets {
    #[serde(default)]
    zip: Option<ManifestAsset>,
    #[serde(default)]
    dmg: Option<ManifestAsset>,
    #[serde(default)]
    tar: Option<ManifestAsset>,
    #[serde(default)]
    appimage: Option<ManifestAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestAsset {
    name: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

impl Release {
    fn from_manifest(m: Manifest, source: &str) -> Result<Release> {
        let version = m.version.trim().trim_start_matches('v').to_string();
        if version.is_empty() {
            bail!("the update manifest has no version");
        }

        let notes_url = m
            .notes_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| RELEASES_URL.to_string());

        let selected = select_platform_assets(&m);
        let (primary, fallback) = build_artifact(&selected, source);

        Ok(Release {
            version,
            notes_url,
            primary,
            fallback,
        })
    }
}

/// Choose the asset set for the running platform: the matching `platforms`
/// entry, or — for manifests written before `platforms` existed — the legacy
/// top-level macOS assets when running on macOS.
fn select_platform_assets(m: &Manifest) -> PlatformAssets {
    if let Some(entry) = m.platforms.get(platform::slug()) {
        return entry.clone();
    }
    if platform::os() == Os::Macos {
        return PlatformAssets {
            zip: m.zip.clone(),
            dmg: m.dmg.clone(),
            ..PlatformAssets::default()
        };
    }
    PlatformAssets::default()
}

/// Turn the selected assets into this platform's primary artifact and optional
/// manual fallback.
fn build_artifact(sel: &PlatformAssets, source: &str) -> (Option<Artifact>, Option<Asset>) {
    let art = |a: &ManifestAsset, kind: ArtifactKind| {
        Some(Artifact {
            asset: asset_from(a.clone(), source),
            kind,
        })
    };
    match platform::os() {
        Os::Macos => (
            sel.zip.as_ref().and_then(|a| art(a, ArtifactKind::MacApp)),
            sel.dmg.as_ref().map(|a| asset_from(a.clone(), source)),
        ),
        Os::Linux => (
            sel.appimage
                .as_ref()
                .and_then(|a| art(a, ArtifactKind::UnixAppImage))
                .or_else(|| {
                    sel.tar
                        .as_ref()
                        .and_then(|a| art(a, ArtifactKind::UnixBinaryTar))
                }),
            None,
        ),
        // Windows packaging ships with the Windows item.
        Os::Windows => (None, None),
    }
}

fn asset_from(a: ManifestAsset, source: &str) -> Asset {
    Asset {
        url: resolve_asset_url(source, &a.name),
        sha256: a
            .sha256
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| is_sha256_hex(s)),
        name: a.name,
        size: a.size,
    }
}

/// Fetch the newest release from `$TEXTLAYR_UPDATE_URL` or the default
/// manifest, falling back to the GitHub API when the manifest doesn't exist
/// (a release cut before `latest.json` shipped). The fallback is skipped when an
/// explicit source override is set, so tests and local servers are authoritative.
pub fn fetch_latest(cancel: &AtomicBool) -> Result<Release> {
    let source = update_source();
    match fetch_manifest(&source, cancel) {
        Ok(release) => Ok(release),
        Err(manifest_err) if using_default_source(&source) => match fetch_from_api(cancel) {
            Ok(release) => Ok(release),
            Err(api_err) => {
                // Both the manifest and the API failed: almost always "offline",
                // or a release with neither a manifest nor published assets. Keep
                // the technical detail on stderr and show one clean line.
                eprintln!("      update check failed: {manifest_err:#}");
                eprintln!("      GitHub API fallback failed: {api_err:#}");
                bail!("couldn't check for updates — no connection, or no release published yet")
            }
        },
        Err(e) => Err(e),
    }
}

fn fetch_manifest(source: &str, cancel: &AtomicBool) -> Result<Release> {
    let raw = read_source(source, cancel)?;
    let manifest: Manifest =
        serde_json::from_slice(&raw).context("unexpected update manifest (not valid JSON)")?;
    Release::from_manifest(manifest, source)
}

fn using_default_source(source: &str) -> bool {
    source == DEFAULT_MANIFEST_URL
}

/// The manifest source: `$TEXTLAYR_UPDATE_URL`, else [`DEFAULT_MANIFEST_URL`].
fn update_source() -> String {
    resolve_source(std::env::var("TEXTLAYR_UPDATE_URL").ok().as_deref())
}

fn resolve_source(override_val: Option<&str>) -> String {
    match override_val {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => DEFAULT_MANIFEST_URL.to_string(),
    }
}

/// Fetch a manifest bytes: from disk for a local path/`file://`, else over HTTP.
fn read_source(source: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    if let Some(path) = local_path(source) {
        return std::fs::read(&path)
            .with_context(|| format!("failed to read update manifest {}", path.display()));
    }
    fetch_bytes(source, cancel, false)
}

/// A local filesystem path for `source`, or `None` for an http(s) URL.
fn local_path(source: &str) -> Option<PathBuf> {
    if let Some(rest) = source.strip_prefix("file://") {
        return Some(PathBuf::from(rest));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return None;
    }
    Some(PathBuf::from(source))
}

/// Resolve an asset's download URL as a sibling of the manifest `source`:
/// `…/dir/latest.json` + `App.zip` → `…/dir/App.zip`. This is what lets the
/// exact same manifest work from GitHub and from a local directory.
fn resolve_asset_url(source: &str, name: &str) -> String {
    if let Some(base) = local_path(source) {
        let dir = base.parent().unwrap_or_else(|| Path::new("."));
        let joined = dir.join(name);
        let abs = if joined.is_absolute() {
            joined
        } else {
            std::env::current_dir().unwrap_or_default().join(joined)
        };
        // A `file://` URL so the shared downloader can read it.
        return format!("file://{}", abs.to_string_lossy());
    }
    match source.rfind('/') {
        Some(i) => format!("{}/{}", &source[..i], name),
        None => name.to_string(),
    }
}

/// Fetch a manifest or GitHub API response through the shared downloader, so
/// the updater and the installer speak one transport and neither shells out.
fn fetch_bytes(url: &str, cancel: &AtomicBool, api: bool) -> Result<Vec<u8>> {
    let accept = api.then_some("application/vnd.github+json");
    download::get_bytes(url, cancel, accept)
}

// ── GitHub API fallback ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
    /// `sha256:<hex>` when GitHub computed one for the uploaded asset.
    #[serde(default)]
    digest: Option<String>,
}

/// Build a [`Release`] from the GitHub REST API. Used only when the manifest is
/// unavailable (e.g. the newest release predates it).
fn fetch_from_api(cancel: &AtomicBool) -> Result<Release> {
    let raw = fetch_bytes(API_LATEST, cancel, true)?;
    let gh: GhRelease =
        serde_json::from_slice(&raw).context("unexpected response from GitHub (not a release)")?;
    if gh.draft || gh.prerelease {
        bail!("the newest GitHub release is a draft or pre-release");
    }
    let version = gh.tag_name.trim().trim_start_matches('v').to_string();
    if version.is_empty() {
        bail!("the newest GitHub release has no version tag");
    }

    let mut sums_url = None;
    let mut assets: Vec<Asset> = gh
        .assets
        .into_iter()
        .map(|a| {
            let lower = a.name.to_ascii_lowercase();
            if lower == "sha256sums" || lower == "sha256sums.txt" {
                sums_url = Some(a.browser_download_url.clone());
            }
            Asset {
                sha256: a.digest.as_deref().and_then(parse_digest),
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
            }
        })
        .collect();

    let zip_idx = assets.iter().position(|a| is_app_asset(&a.name, ".zip"));
    let dmg_idx = assets.iter().position(|a| is_app_asset(&a.name, ".dmg"));

    // Older releases may predate GitHub's per-asset `digest`; fill the hash from
    // a published SHA256SUMS when we can (best effort — the manual-install path
    // tolerates a missing checksum).
    if let Some(i) = zip_idx {
        if assets[i].sha256.is_none() {
            if let Some(url) = sums_url.as_deref() {
                if let Ok(text) = fetch_bytes(url, cancel, false) {
                    if let Ok(text) = String::from_utf8(text) {
                        assets[i].sha256 = parse_sums(&text, &assets[i].name);
                    }
                }
            }
        }
    }

    let notes_url = if gh.html_url.trim().is_empty() {
        RELEASES_URL.to_string()
    } else {
        gh.html_url
    };

    // The REST fallback only understands the legacy macOS layout; on other
    // platforms it can still report a version, just with nothing to install.
    let primary = if platform::os() == Os::Macos {
        zip_idx.map(|i| Artifact {
            asset: assets[i].clone(),
            kind: ArtifactKind::MacApp,
        })
    } else {
        None
    };
    let fallback = dmg_idx.map(|i| assets[i].clone());
    Ok(Release {
        version,
        notes_url,
        primary,
        fallback,
    })
}

/// Whether `name` looks like one of our release artifacts with the given suffix.
fn is_app_asset(name: &str, ext: &str) -> bool {
    name.starts_with("TextLayr") && name.to_ascii_lowercase().ends_with(ext)
}

/// Parse a `sha256:` (or `sha256=`) digest into lowercase hex.
fn parse_digest(digest: &str) -> Option<String> {
    let (algo, hex) = digest.split_once([':', '='])?;
    if !algo.eq_ignore_ascii_case("sha256") {
        return None;
    }
    let hex = hex.trim().to_ascii_lowercase();
    is_sha256_hex(&hex).then_some(hex)
}

/// Find the lowercase hex hash for `name` in a `SHA256SUMS` file.
fn parse_sums(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(hash), Some(file)) = (it.next(), it.next()) else {
            continue;
        };
        let file = file.trim_start_matches('*');
        if file == name && is_sha256_hex(hash) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

// ── version comparison ─────────────────────────────────────────────────────

/// Parse a dotted numeric version ("0.2.0", "v1.4", "0.3.0-rc1") into up to
/// three components. A `-`/`+` suffix (pre-release / build metadata) is ignored
/// for ordering. `None` when there is no leading numeric component.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next().unwrap_or(s);
    if core.is_empty() {
        return None;
    }
    let mut nums = [0u64; 3];
    let mut seen = false;
    for (i, part) in core.split('.').take(3).enumerate() {
        if part.is_empty() {
            break;
        }
        match part.parse::<u64>() {
            Ok(n) => {
                nums[i] = n;
                seen = true;
            }
            Err(_) => return None,
        }
    }
    seen.then_some((nums[0], nums[1], nums[2]))
}

/// True when `candidate` is strictly newer than `current`. Unparseable input is
/// treated as "not newer" so a malformed tag can never trigger an update.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(c), Some(n)) => n > c,
        _ => false,
    }
}

// ── install ────────────────────────────────────────────────────────────────

/// Progress of an update download/install, for a progress bar.
#[derive(Debug, Clone)]
pub enum InstallProgress {
    /// Bytes received (and the expected total when known).
    Downloading { got: u64, total: Option<u64> },
    /// Unpacking the archive.
    Extracting,
    /// Checking the extracted bundle (identifier, version, signature).
    Validating,
    /// Handing off to the relaunch helper.
    Launching,
    /// Self-update wasn't possible; opening the installer for a manual drag.
    OpeningDmg,
}

/// Progress sink for [`install`].
pub type InstallSink = Box<dyn FnMut(InstallProgress) + Send>;

/// How an update concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The bundle will be swapped by a helper once this process exits; the app
    /// should quit so it can relaunch.
    SelfUpdate,
    /// Self-update wasn't possible here; the installer/file was opened so the
    /// user can install it themselves.
    OpenedDmg,
}

/// Download `release` and either swap the running bundle in place (`SelfUpdate`)
/// or open the installer for a manual install (`OpenedDmg`).
pub fn install(
    release: &Release,
    cancel: &AtomicBool,
    sink: &mut InstallSink,
) -> Result<InstallOutcome> {
    let staging = staging_dir(&release.version)?;
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create {}", staging.display()))?;

    // Only a real, writable `.app` (not a dev checkout, not a translocated
    // quarantined copy) can be replaced in place.
    let target = current_bundle();
    let self_update = target
        .as_deref()
        .is_some_and(|app| cfg!(target_os = "macos") && can_self_update(app));

    if !self_update {
        return open_installer(release, &staging, cancel, sink);
    }
    let target = target.expect("checked above");

    let art = release
        .primary
        .as_ref()
        .filter(|a| a.kind == ArtifactKind::MacApp)
        .context("this release has no macOS app archive")?;
    let zip = &art.asset;
    let sha = zip
        .sha256
        .clone()
        .with_context(|| format!("no checksum published for {}", zip.name))?;

    sink(InstallProgress::Downloading {
        got: 0,
        total: zip.size,
    });
    let zip_path = staging.join(&zip.name);
    let mut byte_sink = |got: u64, total: Option<u64>| {
        sink(InstallProgress::Downloading {
            got,
            total: total.or(zip.size),
        });
    };
    download::fetch_verified(&zip.url, &zip_path, &sha, zip.size, cancel, &mut byte_sink)
        .with_context(|| format!("failed to download {}", zip.name))?;

    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }

    // Unpack with `ditto`, which round-trips the bundle (resource forks, code
    // signature) faithfully. macOS only: a Linux/Windows run takes the
    // open-the-installer path above instead.
    sink(InstallProgress::Extracting);
    let extracted = staging.join("extracted");
    std::fs::create_dir_all(&extracted)?;
    let status = Command::new(DITTO)
        .arg("-x")
        .arg("-k")
        .arg(&zip_path)
        .arg(&extracted)
        .status()
        .with_context(|| format!("failed to run {DITTO}"))?;
    if !status.success() {
        bail!("failed to extract {}", zip.name);
    }

    let new_app =
        find_app(&extracted).context("the downloaded archive does not contain TextLayr.app")?;

    sink(InstallProgress::Validating);
    validate_bundle(&new_app, &release.version)?;
    strip_quarantine(&new_app);

    sink(InstallProgress::Launching);
    let script = write_helper(&staging, &target, &new_app)?;
    spawn_detached(&script)?;

    Ok(InstallOutcome::SelfUpdate)
}

/// When self-update isn't possible: download the disk image (or the zip) to the
/// user's Downloads folder and open it so they can install by hand.
fn open_installer(
    release: &Release,
    staging: &Path,
    cancel: &AtomicBool,
    sink: &mut InstallSink,
) -> Result<InstallOutcome> {
    sink(InstallProgress::OpeningDmg);
    // Prefer the manual fallback (the macOS `.dmg`); otherwise hand over this
    // platform's primary artifact. Linux never self-replaces yet, so a `.tar.*`
    // or AppImage lands next to the user for them to install.
    let (asset, is_dmg) = match (&release.fallback, &release.primary) {
        (Some(fallback), _) => (
            fallback.clone(),
            fallback.name.to_ascii_lowercase().ends_with(".dmg"),
        ),
        (None, Some(primary)) => (primary.asset.clone(), false),
        (None, None) => bail!("this release has nothing to download for your platform"),
    };

    let dest_dir = dirs::download_dir()
        .filter(|d| d.is_dir())
        .unwrap_or_else(|| staging.to_path_buf());
    let dest = unique_path(&dest_dir, &asset.name);

    let mut byte_sink = |got: u64, total: Option<u64>| {
        sink(InstallProgress::Downloading {
            got,
            total: total.or(asset.size),
        });
    };
    match asset.sha256.clone() {
        Some(sha) => {
            download::fetch_verified(&asset.url, &dest, &sha, asset.size, cancel, &mut byte_sink)
                .with_context(|| format!("failed to download {}", asset.name))?;
        }
        None => {
            // No published checksum: still fine to fetch for a manual install —
            // the user drags it in, and Gatekeeper stays in the loop.
            let total = asset.size.or_else(|| download::content_length(&asset.url));
            download::download_to(&asset.url, &dest, total, cancel, &mut byte_sink)
                .with_context(|| format!("failed to download {}", asset.name))?;
        }
    }
    strip_quarantine(&dest);

    if is_dmg && cfg!(target_os = "macos") {
        Command::new(OPEN)
            .arg(&dest)
            .spawn()
            .context("failed to open the disk image")?;
    } else {
        let _ = crate::platform::reveal_in_file_manager(&dest);
    }
    Ok(InstallOutcome::OpenedDmg)
}

// ── bundle handling ────────────────────────────────────────────────────────

/// The running `.app` bundle, if the executable lives inside one.
fn current_bundle() -> Option<PathBuf> {
    bundle_of(&paths::current_exe()?)
}

/// `.../TextLayr.app/Contents/MacOS/TextLayr` → `.../TextLayr.app`.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    if app.extension()? != "app" {
        return None;
    }
    Some(app.to_path_buf())
}

/// Whether the running bundle may be replaced in place: not translocated by
/// Gatekeeper and with a writable parent directory.
fn can_self_update(app: &Path) -> bool {
    if app.to_string_lossy().contains("/AppTranslocation/") {
        return false;
    }
    // Replacing the bundle requires renaming inside its parent directory.
    match app.parent() {
        Some(dir) => dir_writable(dir),
        None => false,
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".textlayr-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Find a `TextLayr.app` anywhere under `root`.
fn find_app(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == APP_NAME) {
                    return Some(path);
                }
                stack.push(path);
            }
        }
    }
    None
}

/// Confirm the downloaded bundle is the app we expect before trusting it.
fn validate_bundle(app: &Path, expected_version: &str) -> Result<()> {
    let info = app.join("Contents").join("Info.plist");
    let text =
        std::fs::read_to_string(&info).with_context(|| format!("missing {}", info.display()))?;

    let id = plist_string(&text, "CFBundleIdentifier").unwrap_or_default();
    if id != BUNDLE_ID {
        bail!("downloaded app has an unexpected identifier '{id}'");
    }
    let version = plist_string(&text, "CFBundleShortVersionString").unwrap_or_default();
    if version != expected_version {
        bail!("downloaded app is version '{version}', expected '{expected_version}'");
    }

    // Ad-hoc signatures verify fine with `codesign --verify`; only a broken or
    // absent signature fails. Skipped off macOS (dev checkouts) or via
    // `$TEXTLAYR_UPDATE_SKIP_CODESIGN`.
    if !skip_codesign() && cfg!(target_os = "macos") && Path::new(CODESIGN).exists() {
        let status = Command::new(CODESIGN)
            .args(["--verify", "--deep", "--strict"])
            .arg(app)
            .status()
            .context("failed to run codesign")?;
        if !status.success() {
            bail!("the downloaded app failed codesign verification");
        }
    }
    Ok(())
}

/// Read a `<string>` value for `key` out of an XML plist. Handles the simple
/// key/string pairs that Cocoa bundles use for identifiers and versions.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let needle = format!("<key>{key}</key>");
    let after = &plist[plist.find(&needle)? + needle.len()..];
    let start = after.find("<string>")? + "<string>".len();
    let tail = &after[start..];
    let end = tail.find("</string>")?;
    Some(tail[..end].trim().to_string())
}

/// `<data dir>/updates/<version>`, where a download is staged.
fn staging_dir(version: &str) -> Result<PathBuf> {
    let base = paths::data_dir().context("no application data directory")?;
    Ok(base.join("updates").join(version))
}

/// Write the detached relaunch helper that swaps the bundle once we exit.
fn write_helper(staging: &Path, target_app: &Path, new_app: &Path) -> Result<PathBuf> {
    let script = staging.join("textlayr-update.sh");
    let body = format!(
        r#"#!/bin/sh
# TextLayr self-update helper (generated). Waits for the running app to exit,
# swaps the bundle, strips quarantine and relaunches. Safe to delete.
pid={pid}
app={app}
new={new}
staging={staging}

i=0
while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 900 ]; do
    /bin/sleep 0.2
    i=$((i + 1))
done

backup="${{app}}.old-$$"
/bin/rm -rf "$backup" 2>/dev/null || true
# Same-directory rename (fast, atomic) to get the old bundle out of the way.
if ! /bin/mv "$app" "$backup" 2>/dev/null; then
    /bin/rm -rf "$staging"
    /bin/rm -f "$0"
    exit 1
fi
# Copy the new bundle in with `ditto` so it survives the cross-volume move and
# keeps its metadata/signature; roll back on failure.
if ! /usr/bin/ditto "$new" "$app"; then
    /bin/rm -rf "$app"
    /bin/mv "$backup" "$app" 2>/dev/null || true
    /bin/rm -rf "$staging"
    /bin/rm -f "$0"
    exit 1
fi
/usr/bin/xattr -dr com.apple.quarantine "$app" 2>/dev/null || true
/bin/rm -rf "$backup"
/usr/bin/open "$app"
/bin/rm -rf "$staging"
/bin/rm -f "$0"
"#,
        pid = std::process::id(),
        app = sh_quote(&target_app.to_string_lossy()),
        new = sh_quote(&new_app.to_string_lossy()),
        staging = sh_quote(&staging.to_string_lossy()),
    );
    std::fs::write(&script, body).with_context(|| format!("write {}", script.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms)?;
    }
    Ok(script)
}

/// Run the helper detached so it outlives this process.
fn spawn_detached(script: &Path) -> Result<()> {
    Command::new("/bin/sh")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {}", script.display()))?;
    Ok(())
}

/// Single-quote a path for `/bin/sh`, escaping embedded single quotes.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn strip_quarantine(path: &Path) {
    let _ = Command::new(XATTR)
        .arg("-dr")
        .arg("com.apple.quarantine")
        .arg(path)
        .status();
}

/// A path in `dir` for `name` that doesn't already exist ("name (1).zip").
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = path.extension().and_then(|s| s.to_str());
    for i in 1..1000 {
        let candidate = match ext {
            Some(ext) => dir.join(format!("{stem} ({i}).{ext}")),
            None => dir.join(format!("{stem} ({i})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, sha: &str, size: u64) -> serde_json::Value {
        serde_json::json!({"name": name, "sha256": sha, "size": size})
    }

    #[cfg(not(windows))]
    #[test]
    fn the_platforms_entry_is_selected_and_resolved_as_a_sibling() {
        let sha = "d".repeat(64);
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "version": "v0.3.0",
            "notes_url": "https://example/notes",
            "platforms": {
                platform::slug(): {
                    "zip": asset("TextLayr-0.3.0.zip", &sha, 20),
                    "dmg": asset("TextLayr-0.3.0.dmg", &sha, 30),
                    "tar": asset("TextLayr-0.3.0-linux.tar.xz", &sha, 40),
                    "appimage": asset("TextLayr-0.3.0.AppImage", &sha, 50),
                }
            }
        }))
        .unwrap();

        let release = Release::from_manifest(
            manifest,
            "https://github.com/o/r/releases/latest/download/latest.json",
        )
        .unwrap();
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.notes_url, "https://example/notes");

        let primary = release.primary.expect("the host platform has an artifact");
        let expected = match platform::os() {
            Os::Macos => ArtifactKind::MacApp,
            _ => ArtifactKind::UnixAppImage,
        };
        assert_eq!(primary.kind, expected);
        assert_eq!(
            primary.asset.url,
            format!(
                "https://github.com/o/r/releases/latest/download/{}",
                primary.asset.name
            )
        );
        assert_eq!(primary.asset.sha256.as_deref(), Some(sha.as_str()));
        // Only macOS publishes a manual fallback (the .dmg).
        assert_eq!(release.fallback.is_some(), platform::os() == Os::Macos);
    }

    #[test]
    fn a_legacy_manifest_only_serves_macos() {
        let sha = "e".repeat(64);
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "version": "0.3.0",
            "notes_url": "https://example/notes",
            "zip": asset("TextLayr-0.3.0.zip", &sha, 20),
            "dmg": asset("TextLayr-0.3.0.dmg", &sha, 30),
        }))
        .unwrap();
        let release = Release::from_manifest(manifest, "https://h/d/latest.json").unwrap();
        assert_eq!(release.notes_url, "https://example/notes");
        if platform::os() == Os::Macos {
            assert_eq!(release.primary.unwrap().kind, ArtifactKind::MacApp);
            assert!(release.fallback.is_some());
        } else {
            assert!(release.primary.is_none(), "legacy assets are macOS-only");
            assert!(release.fallback.is_none());
        }
    }

    #[test]
    fn manifest_without_notes_url_falls_back_to_the_releases_page() {
        let manifest: Manifest =
            serde_json::from_value(serde_json::json!({"version": "0.3.0"})).unwrap();
        let release = Release::from_manifest(manifest, "https://h/d/latest.json").unwrap();
        assert_eq!(release.notes_url, RELEASES_URL);
    }

    #[test]
    fn manifest_without_a_version_is_rejected() {
        let m: Manifest = serde_json::from_value(serde_json::json!({"version": "  "})).unwrap();
        assert!(Release::from_manifest(m, "https://h/latest.json").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_prefers_the_appimage_then_the_tarball() {
        let sha = "c".repeat(64);
        let appimage = ManifestAsset {
            name: "TextLayr-0.3.0.AppImage".into(),
            sha256: Some(sha.clone()),
            size: Some(50),
        };
        let tar = ManifestAsset {
            name: "TextLayr-0.3.0-linux.tar.xz".into(),
            sha256: Some(sha),
            size: Some(40),
        };
        let both = PlatformAssets {
            appimage: Some(appimage),
            tar: Some(tar.clone()),
            ..PlatformAssets::default()
        };
        let (primary, fallback) = build_artifact(&both, "https://h/latest.json");
        assert_eq!(primary.unwrap().kind, ArtifactKind::UnixAppImage);
        assert!(fallback.is_none());

        let only_tar = PlatformAssets {
            tar: Some(tar),
            ..PlatformAssets::default()
        };
        let (primary, _) = build_artifact(&only_tar, "https://h/latest.json");
        assert_eq!(primary.unwrap().kind, ArtifactKind::UnixBinaryTar);
    }

    #[test]
    fn local_manifest_resolves_assets_into_file_urls() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("latest.json");
        let url = resolve_asset_url(manifest.to_str().unwrap(), "TextLayr-0.3.0.zip");
        assert!(url.starts_with("file://"), "{url}");
        assert!(url.ends_with("/TextLayr-0.3.0.zip"), "{url}");
        assert!(url.contains(dir.path().to_str().unwrap()), "{url}");
    }

    #[test]
    fn source_override_defaults_to_the_github_manifest() {
        assert_eq!(resolve_source(None), DEFAULT_MANIFEST_URL);
        assert_eq!(resolve_source(Some("  ")), DEFAULT_MANIFEST_URL);
        assert_eq!(
            resolve_source(Some("http://127.0.0.1:8000/latest.json")),
            "http://127.0.0.1:8000/latest.json"
        );
        assert!(using_default_source(DEFAULT_MANIFEST_URL));
        assert!(!using_default_source("http://127.0.0.1:8000/latest.json"));
    }

    #[test]
    fn digests_parse_only_as_sha256() {
        let hex = "a".repeat(64);
        assert_eq!(parse_digest(&format!("sha256:{hex}")), Some(hex.clone()));
        assert_eq!(parse_digest(&format!("SHA256={hex}")), Some(hex.clone()));
        assert_eq!(parse_digest("md5:abcd"), None);
        assert_eq!(parse_digest("sha256:short"), None);
    }

    #[test]
    fn sums_lookup_matches_the_asset_name() {
        let hex = "b".repeat(64);
        let text =
            format!("deadbeef  Other.zip\n{hex}  TextLayr-0.3.0.zip\n{hex}  *TextLayr-0.3.0.dmg\n");
        assert_eq!(parse_sums(&text, "TextLayr-0.3.0.zip"), Some(hex.clone()));
        assert_eq!(parse_sums(&text, "TextLayr-0.3.0.dmg"), Some(hex.clone()));
        assert_eq!(parse_sums(&text, "missing.zip"), None);
    }

    #[test]
    fn github_api_release_is_recognised() {
        let sha = "c".repeat(64);
        let gh: GhRelease = serde_json::from_str(&format!(
            r#"{{
              "tag_name": "v0.2.0",
              "html_url": "https://github.com/o/r/releases/tag/v0.2.0",
              "assets": [
                {{"name":"TextLayr-0.2.0.dmg","browser_download_url":"https://o/a.dmg","size":10,
                  "digest":"sha256:{sha}"}},
                {{"name":"TextLayr-0.2.0.zip","browser_download_url":"https://o/a.zip","size":20}}
              ]
            }}"#
        ))
        .unwrap();
        assert_eq!(gh.tag_name, "v0.2.0");
        assert!(is_app_asset("TextLayr-0.2.0.zip", ".zip"));
        assert!(!is_app_asset("SHA256SUMS", ".zip"));
    }

    #[test]
    fn version_parsing_accepts_the_shapes_we_publish() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v1.4.2"), Some((1, 4, 2)));
        assert_eq!(parse_version("2.0"), Some((2, 0, 0)));
        assert_eq!(parse_version("0.3.0-rc1"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.3.0+meta"), Some((0, 3, 0)));
        assert_eq!(parse_version("nope"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn newer_versions_are_detected_and_never_regressed() {
        assert!(is_newer("0.2.0", "0.2.1"));
        assert!(is_newer("0.2.0", "v0.3.0"));
        assert!(is_newer("0.2.0", "1.0.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.3.0", "0.2.9"));
        // Malformed input must never trigger an update.
        assert!(!is_newer("0.2.0", "garbage"));
        assert!(!is_newer("garbage", "9.9.9"));
    }

    #[test]
    fn plist_reader_finds_identifier_and_version() {
        let plist = r#"<dict>
            <key>CFBundleIdentifier</key><string>com.paulbuitenhuis.textlayr</string>
            <key>CFBundleShortVersionString</key>
            <string>0.3.0</string>
        </dict>"#;
        assert_eq!(
            plist_string(plist, "CFBundleIdentifier").as_deref(),
            Some("com.paulbuitenhuis.textlayr")
        );
        assert_eq!(
            plist_string(plist, "CFBundleShortVersionString").as_deref(),
            Some("0.3.0")
        );
        assert_eq!(plist_string(plist, "Missing"), None);
    }

    #[test]
    fn bundle_is_recognised_from_the_executable_path() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/TextLayr.app/Contents/MacOS/TextLayr"
            )),
            Some(PathBuf::from("/Applications/TextLayr.app"))
        );
        assert_eq!(bundle_of(Path::new("/tmp/target/debug/textlayr")), None);
        assert_eq!(
            bundle_of(Path::new("/Applications/TextLayr.app/Contents/x")),
            None
        );
    }

    #[test]
    fn current_version_is_a_parseable_version() {
        assert!(parse_version(current_version()).is_some());
    }

    #[test]
    fn helper_script_is_written_with_quoted_paths() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("updates").join("0.3.0");
        std::fs::create_dir_all(&staging).unwrap();
        let target = dir.path().join("Applications").join("TextLayr.app");
        let new = staging.join("extracted").join("TextLayr.app");

        let script = write_helper(&staging, &target, &new).unwrap();
        assert!(script.is_file());
        let body = std::fs::read_to_string(&script).unwrap();
        // Paths are single-quoted so spaces survive.
        assert!(body.contains(&format!("'{}'", target.display())));
        assert!(body.contains(&format!("'{}'", new.display())));
        assert!(body.contains("/usr/bin/ditto"));
        assert!(body.contains("com.apple.quarantine"));
        assert!(body.contains(r#"/bin/rm -f "$0""#));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "helper should be executable");
        }
    }

    #[test]
    fn unique_path_avoids_clobbering_an_existing_download() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("TextLayr-0.3.0.dmg");
        assert_eq!(unique_path(dir.path(), "TextLayr-0.3.0.dmg"), first);
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "TextLayr-0.3.0.dmg"),
            dir.path().join("TextLayr-0.3.0 (1).dmg")
        );
    }
}
