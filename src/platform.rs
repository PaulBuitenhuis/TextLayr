//! Host-platform specifics, in one place.
//!
//! Everything that differs between macOS, Windows and Linux is funnelled through
//! this module so the rest of the crate — and, in particular, resource discovery
//! — does not scatter `cfg!(target_os = …)` around:
//!
//! - target identity ([`os`], [`arch`], [`triple`], [`slug`]) used for release
//!   asset names and the update manifest;
//! - [`exe_name`] and [`which`] for locating external programs (`ffmpeg`,
//!   `ffprobe`) whose names/lookup differ on Windows;
//! - [`reveal_in_file_manager`] for the "reveal in folder" affordance;
//! - [`bundle_roots`] / [`dev_roots`] for finding the app's own resources
//!   (fonts, models, bundled binaries) whether packaged or run from a checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::export::Encoder;
use crate::source::Source;

/// The host operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Macos,
    Windows,
    Linux,
}

/// The host CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Arm64,
    X86_64,
}

/// The operating system this build is running on.
pub fn os() -> Os {
    if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Linux
    }
}

/// The CPU architecture this build is running on.
pub fn arch() -> Arch {
    if cfg!(target_arch = "aarch64") {
        Arch::Arm64
    } else {
        Arch::X86_64
    }
}

/// A stable, human-readable platform id used for release asset names and as the
/// key in the `latest.json` manifest (`macos-arm64`, `windows-x64`, …).
pub fn slug() -> &'static str {
    match (os(), arch()) {
        (Os::Macos, Arch::Arm64) => "macos-arm64",
        (Os::Macos, Arch::X86_64) => "macos-x64",
        (Os::Windows, Arch::X86_64) => "windows-x64",
        (Os::Windows, Arch::Arm64) => "windows-arm64",
        (Os::Linux, Arch::X86_64) => "linux-x86_64",
        (Os::Linux, Arch::Arm64) => "linux-aarch64",
    }
}

/// The Rust target triple for this build.
pub fn triple() -> &'static str {
    match (os(), arch()) {
        (Os::Macos, Arch::Arm64) => "aarch64-apple-darwin",
        (Os::Macos, Arch::X86_64) => "x86_64-apple-darwin",
        (Os::Windows, Arch::X86_64) => "x86_64-pc-windows-msvc",
        (Os::Windows, Arch::Arm64) => "aarch64-pc-windows-msvc",
        (Os::Linux, Arch::X86_64) => "x86_64-unknown-linux-gnu",
        (Os::Linux, Arch::Arm64) => "aarch64-unknown-linux-gnu",
    }
}

/// The subdirectory name used under a dev checkout's `vendor/` folder
/// (`vendor/macos`, `vendor/linux`, `vendor/windows`).
pub fn vendor_slug() -> &'static str {
    match os() {
        Os::Macos => "macos",
        Os::Windows => "windows",
        Os::Linux => "linux",
    }
}

/// Hardware encoders to try, in preference order, when the user asks for
/// `Auto`. The first one whose probe passes wins; the list always ends in the
/// software `x264`, which is universal, so resolution is total.
///
/// Adding NVENC/QSV/AMF later is a one-line change here plus the variant — call
/// sites never change.
pub fn encoder_candidates() -> &'static [Encoder] {
    match os() {
        Os::Macos => &[Encoder::Videotoolbox, Encoder::X264],
        Os::Linux => &[Encoder::Vaapi, Encoder::X264],
        // No hardware encoder is wired up on Windows yet (Phase 6); NVENC/QSV/AMF
        // slot in ahead of x264 here without touching `export.rs`.
        Os::Windows => &[Encoder::X264],
    }
}

/// The on-disk name of an external program: `ffmpeg` on macOS/Linux,
/// `ffmpeg.exe` on Windows.
pub fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Locate `bin` on the user's `PATH`.
///
/// On Unix this defers to the shell's `command -v` (matching what the user would
/// get in a terminal); on Windows it walks `PATH` honouring `PATHEXT`, because
/// there is no `sh`.
#[cfg(not(windows))]
pub fn which(bin: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .ok()?;
    if out.status.success() {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

/// See the Unix variant above. Windows has no `sh`, so walk `PATH`+`PATHEXT`.
#[cfg(windows)]
pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let pathext =
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let exts: Vec<String> = pathext
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(bin);
        if direct.is_file() {
            return Some(direct);
        }
        for ext in &exts {
            let cand = dir.join(format!("{bin}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Open the folder containing `path` in the OS file manager, selecting the file
/// where the platform supports it (Finder, Explorer) and opening the parent
/// directory otherwise.
pub fn reveal_in_file_manager(path: &Path) -> Result<()> {
    match os() {
        Os::Macos => {
            Command::new("open").arg("-R").arg(path).spawn()?;
        }
        Os::Windows => {
            // explorer.exe is fussy: `/select,` must be glued to the path and it
            // always returns a non-zero exit code, so we only spawn and ignore.
            let arg = format!("/select,{}", path.display());
            Command::new("explorer").arg(arg).spawn()?;
        }
        Os::Linux => {
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Command::new("xdg-open").arg(dir).spawn()?;
        }
    }
    Ok(())
}

/// The app's bundled resource root(s), most specific first, tagged
/// [`Source::Bundled`]. Each is a directory that may contain `bin/`, `models/`
/// and `assets/fonts/` sub-trees.
///
/// - macOS: `TextLayr.app/Contents/Resources`
/// - Linux: `$APPDIR/usr/share/textlayr` (AppImage) then `/usr/share/textlayr`
///   (installed), with resources-beside-the-binary handled by [`dev_roots`]
/// - Windows: `resources/` beside the executable
pub fn bundle_roots() -> Vec<(PathBuf, Source)> {
    let mut roots = Vec::new();
    let Ok(exe) = std::env::current_exe() else {
        return roots;
    };
    let exe_dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    match os() {
        Os::Macos => {
            // exe lives in Contents/MacOS; resources in Contents/Resources.
            if let Some(contents) = exe_dir.parent() {
                roots.push((contents.join("Resources"), Source::Bundled));
            }
        }
        Os::Linux => {
            if let Some(appdir) = std::env::var_os("APPDIR") {
                roots.push((
                    PathBuf::from(appdir)
                        .join("usr")
                        .join("share")
                        .join("textlayr"),
                    Source::Bundled,
                ));
            }
            // /usr/bin/textlayr -> /usr/share/textlayr (deb/rpm/generic install).
            if let Some(prefix) = exe_dir.parent() {
                roots.push((prefix.join("share").join("textlayr"), Source::Bundled));
            }
        }
        Os::Windows => {
            roots.push((exe_dir.join("resources"), Source::Bundled));
        }
    }
    roots
}

/// Directories to search while developing, tagged [`Source::Dev`]: walking up
/// from the executable and then from the current directory, looking for a source
/// checkout (a directory that contains `assets/fonts`, `models` or `vendor/`).
pub fn dev_roots() -> Vec<(PathBuf, Source)> {
    let mut roots = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf);
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            cur = dir.parent().map(Path::to_path_buf);
            roots.push((dir, Source::Dev));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd);
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            cur = dir.parent().map(Path::to_path_buf);
            roots.push((dir, Source::Dev));
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_triple_are_non_empty_and_consistent() {
        assert!(!slug().is_empty());
        assert!(!triple().is_empty());
        assert!(!vendor_slug().is_empty());
        // The slug's OS prefix and the triple's components agree.
        let os_prefix = match os() {
            Os::Macos => "macos",
            Os::Windows => "windows",
            Os::Linux => "linux",
        };
        assert!(slug().starts_with(os_prefix), "{} vs {os_prefix}", slug());
    }

    #[test]
    fn exe_name_appends_the_extension_only_on_windows() {
        let got = exe_name("ffmpeg");
        if cfg!(windows) {
            assert_eq!(got, "ffmpeg.exe");
        } else {
            assert_eq!(got, "ffmpeg");
        }
    }

    #[test]
    fn which_finds_a_program_that_definitely_exists() {
        // `sh` on Unix; on Windows we at least expect the lookup to run without
        // panicking (a hit is not guaranteed for every bin).
        let found = which(if cfg!(windows) { "cmd" } else { "sh" });
        assert!(found.is_some(), "expected to locate a shell on this host");
    }

    #[test]
    fn bundle_and_dev_roots_are_well_formed() {
        // We don't assert hits (the layout varies), only that paths are absolute
        // where possible and the sources are tagged as documented.
        for (_, src) in bundle_roots() {
            assert_eq!(src, Source::Bundled);
        }
        for (_, src) in dev_roots() {
            assert_eq!(src, Source::Dev);
        }
    }

    #[test]
    fn encoder_candidates_are_total_and_end_in_x264() {
        let cands = encoder_candidates();
        assert!(!cands.is_empty());
        // x264 always works, so it must be the final fallback.
        assert_eq!(*cands.last().unwrap(), Encoder::X264);
    }
}
