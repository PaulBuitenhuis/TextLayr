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
//!
//! The `*_for` variants take the executable and current directory explicitly so
//! discovery can be tested without depending on where the test binary lives.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

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
/// Walks `PATH` rather than shelling out to `command -v`, so a missing tool is
/// reported the same way everywhere (and no `sh` is spawned during a startup
/// dependency check). On Windows the walk honours `PATHEXT`.
pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    which_in(&path, bin)
}

fn which_in(path_var: &std::ffi::OsStr, bin: &str) -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        for dir in std::env::split_paths(path_var) {
            let candidate = dir.join(bin);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(windows)]
    {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        let exts: Vec<String> = pathext
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        for dir in std::env::split_paths(path_var) {
            let direct = dir.join(bin);
            if direct.is_file() {
                return Some(direct);
            }
            for ext in &exts {
                let candidate = dir.join(format!("{bin}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }
}

#[cfg(not(windows))]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Open the folder containing `path` in the OS file manager, selecting the file
/// where the platform supports it (Finder, Explorer) and opening the parent
/// directory otherwise.
///
/// The child is reaped on a detached thread: `open`/`xdg-open` exit almost
/// immediately, and leaving them unreaped is how the app used to accumulate
/// zombies until it quit.
pub fn reveal_in_file_manager(path: &Path) -> Result<()> {
    let mut cmd = match os() {
        Os::Macos => {
            let mut c = Command::new("open");
            c.arg("-R").arg(path);
            c
        }
        Os::Windows => {
            // explorer.exe is fussy: `/select,` must be glued to the path, and
            // it always exits non-zero, so we only spawn it.
            let mut c = Command::new("explorer");
            c.arg(format!("/select,{}", path.display()));
            c
        }
        Os::Linux => {
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut c = Command::new("xdg-open");
            // No `--`: most distros ship xdg-open as a shell script that rejects
            // unknown options outright (seen on Ubuntu 26.04). The path is
            // absolute, so it cannot be mistaken for one.
            c.arg(dir);
            c
        }
    };
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The app's bundled resource root(s), most specific first, tagged
/// [`Source::Bundled`]. Each is a directory that may contain `bin/`, `models/`
/// and `assets/fonts/` sub-trees.
///
/// - macOS: `TextLayr.app/Contents/Resources`
/// - Linux: `$APPDIR/usr/share/textlayr` (AppImage) then `<prefix>/share/textlayr`
///   (installed), with resources-beside-the-binary handled by [`dev_roots`]
/// - Windows: `resources/` beside the executable
pub fn bundle_roots() -> Vec<(PathBuf, Source)> {
    bundle_roots_for(crate::paths::current_exe().as_deref())
}

/// [`bundle_roots`] for an explicit executable path (tests, and callers that
/// already resolved it).
pub fn bundle_roots_for(exe: Option<&Path>) -> Vec<(PathBuf, Source)> {
    let mut roots = Vec::new();
    let exe_dir = exe
        .and_then(Path::parent)
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
/// from the executable and then from the current directory.
pub fn dev_roots() -> Vec<(PathBuf, Source)> {
    dev_roots_for(
        crate::paths::current_exe().as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
}

/// [`dev_roots`] for an explicit executable and working directory.
pub fn dev_roots_for(exe: Option<&Path>, cwd: Option<&Path>) -> Vec<(PathBuf, Source)> {
    let mut roots = Vec::new();

    if let Some(dir) = exe.and_then(Path::parent) {
        let mut cur = Some(dir.to_path_buf());
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            cur = dir.parent().map(Path::to_path_buf);
            roots.push((dir, Source::Dev));
        }
    }

    if let Some(cwd) = cwd {
        let mut cur = Some(cwd.to_path_buf());
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
    fn slug_and_triple_agree_with_the_host() {
        assert!(!slug().is_empty());
        assert!(!triple().is_empty());
        assert!(!vendor_slug().is_empty());
        let prefix = match os() {
            Os::Macos => "macos",
            Os::Windows => "windows",
            Os::Linux => "linux",
        };
        assert!(slug().starts_with(prefix), "{} vs {prefix}", slug());
        assert!(
            triple().contains(prefix) || prefix == "macos",
            "{}",
            triple()
        );
    }

    #[test]
    fn exe_name_only_gets_an_extension_on_windows() {
        assert_eq!(
            exe_name("ffmpeg"),
            if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            }
        );
    }

    #[test]
    fn which_finds_a_shell_that_exists() {
        let found = which(if cfg!(windows) { "cmd" } else { "sh" });
        assert!(found.is_some(), "expected to locate a shell on this host");
    }

    #[cfg(not(windows))]
    #[test]
    fn which_skips_non_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("textlayr-fixture-tool");
        std::fs::write(&plain, b"not executable").unwrap();
        let mut path_var = dir.path().as_os_str().to_os_string();
        path_var.push(":");
        path_var.push("/usr/bin:/bin");

        assert_eq!(
            which_in(&path_var, "textlayr-fixture-tool"),
            None,
            "a non-executable file must not be picked up"
        );

        let mut perms = std::fs::metadata(&plain).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plain, perms).unwrap();
        assert_eq!(
            which_in(&path_var, "textlayr-fixture-tool"),
            Some(plain),
            "an executable on PATH should be found"
        );
    }

    #[test]
    fn bundle_roots_are_tagged_and_live_outside_the_binary() {
        let exe = Path::new("/Applications/TextLayr.app/Contents/MacOS/TextLayr");
        for (root, source) in bundle_roots_for(Some(exe)) {
            assert_eq!(source, Source::Bundled);
            assert!(!root.as_os_str().is_empty());
        }
        assert!(bundle_roots_for(None)
            .iter()
            .all(|(_, s)| *s == Source::Bundled));
    }

    #[test]
    fn dev_roots_walk_up_from_both_anchors() {
        let exe = Path::new("/work/textlayr/target/debug/textlayr");
        let roots: Vec<PathBuf> = dev_roots_for(Some(exe), Some(Path::new("/work/textlayr")))
            .into_iter()
            .map(|(p, s)| {
                assert_eq!(s, Source::Dev);
                p
            })
            .collect();
        assert!(
            roots.iter().any(|p| p.ends_with("target/debug")),
            "{roots:?}"
        );
        assert!(
            roots.iter().any(|p| p == Path::new("/work/textlayr")),
            "{roots:?}"
        );
        assert!(dev_roots_for(None, None).is_empty());
    }
}
