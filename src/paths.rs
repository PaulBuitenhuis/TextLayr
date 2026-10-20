//! Platform directories TextLayr owns for files it downloads at runtime.
//!
//! Kept separate from the source checkout so a downloaded binary or model is
//! visible and removable without touching the repo or the `.app` bundle.
//!
//! - macOS: `~/Library/Application Support/textlayr/`
//! - Linux: `~/.local/share/textlayr/`

use std::path::PathBuf;

/// Per-user data directory for TextLayr (`None` if the platform has none).
pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("textlayr"))
}

/// The running executable with symlinks resolved.
///
/// Resource lookup (fonts, bundled ffmpeg, bundled models) is derived from the
/// executable's location, so a `textlayr` symlink on `PATH` must resolve back to
/// the real binary inside the `.app` bundle. `std::env::current_exe()` does not
/// resolve symlinks on macOS, hence this wrapper.
pub fn current_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().map(canonicalize_exe)
}

fn canonicalize_exe(exe: PathBuf) -> PathBuf {
    exe.canonicalize().unwrap_or(exe)
}

/// Where downloaded `ffmpeg`/`ffprobe` live.
pub fn bin_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("bin"))
}

/// Where downloaded whisper models live.
pub fn models_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("models"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::canonicalize_exe;
    use std::path::PathBuf;

    #[test]
    fn canonicalize_exe_resolves_symlinks() {
        let dir = std::env::temp_dir().join(format!("textlayr-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            canonicalize_exe(link),
            real.canonicalize().unwrap_or_else(|_| PathBuf::from(&real))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
