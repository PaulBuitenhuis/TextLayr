//! Export destination planning.
//!
//! One place decides where a finished video may be written, so both front-ends
//! obey the same rules:
//!
//! - the destination is always an `.mp4` file;
//! - it is never the input video (including hard links and symlinks);
//! - an existing file is replaced only when the caller has confirmed it;
//! - the encoder writes a hidden `.<name>.part.mp4` beside the destination and
//!   only a successful encode renames it into place. A failed or cancelled
//!   export therefore never touches an existing file.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Default output path: `<input stem>_textlayr.mp4` next to the input.
pub fn default_output(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string());
    let name = format!("{stem}_textlayr.mp4");
    match input.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

/// A validated export destination plus its in-progress sibling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportTarget {
    final_path: PathBuf,
    part_path: PathBuf,
}

impl ExportTarget {
    /// Validate `requested` (or the default name) for `input`.
    ///
    /// `allow_existing` is the already-confirmed overwrite case: the save
    /// dialog (or the CLI's `--force`) has asked the user. The input guard is
    /// unconditional; even `--force` cannot overwrite the source video.
    pub fn resolve(input: &Path, requested: Option<&Path>, allow_existing: bool) -> Result<Self> {
        let mut final_path = match requested {
            Some(path) => path.to_path_buf(),
            None => default_output(input),
        };

        normalise_extension(&mut final_path)?;

        let Some(name) = final_path.file_name().map(|n| n.to_os_string()) else {
            bail!("the output path has no file name: {}", final_path.display());
        };
        let parent = match final_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if !parent.is_dir() {
            bail!("the output folder does not exist: {}", parent.display());
        }
        if final_path.is_dir() {
            bail!("the output path is a folder: {}", final_path.display());
        }
        if same_file(input, &final_path)? {
            bail!(
                "the output is the input video itself ({}); choose another name",
                input.display()
            );
        }
        if final_path.exists() && !allow_existing {
            bail!(
                "{} already exists; pass --force to replace it",
                final_path.display()
            );
        }

        let part_path = parent.join(format!(".{}.part.mp4", name.to_string_lossy()));
        Ok(ExportTarget {
            final_path,
            part_path,
        })
    }

    /// The path the finished video will have.
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// The hidden file the encoder writes to.
    pub fn part_path(&self) -> &Path {
        &self.part_path
    }

    /// Create (or truncate) the part file, failing before any expensive work if
    /// the destination folder is not writable.
    pub fn prepare(&self) -> Result<()> {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.part_path)
            .with_context(|| format!("cannot write {}", self.part_path.display()))?;
        Ok(())
    }

    /// Move the finished encode into place. The caller has already confirmed
    /// any replacement.
    pub fn commit(&self) -> Result<()> {
        fs::rename(&self.part_path, &self.final_path).with_context(|| {
            format!(
                "cannot move the finished video into place at {}",
                self.final_path.display()
            )
        })
    }

    /// Remove the in-progress file; never touches `final_path`.
    pub fn discard(&self) {
        let _ = fs::remove_file(&self.part_path);
    }
}

/// Append `.mp4` when there is no extension, accept `.mp4` in any case, and
/// reject every other container with a clear message.
///
/// GTK's portal save dialog appends the filter's extension even when the
/// chosen name already ends with it, so a default export arrives as
/// `clip_textlayr.mp4.mp4`. Collapse that to one extension; nobody means to
/// write a file called `*.mp4.mp4`.
fn normalise_extension(path: &mut PathBuf) -> Result<()> {
    match path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("mp4") => {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Some(base) = stem
                    .strip_suffix(".mp4")
                    .or_else(|| stem.strip_suffix(".MP4"))
                {
                    path.set_file_name(format!("{base}.mp4"));
                }
            }
            Ok(())
        }
        Some(ext) if ext.is_empty() => {
            path.set_extension("mp4");
            Ok(())
        }
        Some(ext) => bail!(
            "TextLayr writes MP4 files; choose a .mp4 name instead of .{}",
            ext.to_string_lossy()
        ),
        None => {
            path.set_extension("mp4");
            Ok(())
        }
    }
}

/// Whether `output` names the same file as `input`.
fn same_file(input: &Path, output: &Path) -> Result<bool> {
    let input = fs::canonicalize(input)
        .with_context(|| format!("cannot resolve the input video {}", input.display()))?;

    if output.exists() {
        let resolved = fs::canonicalize(output)
            .with_context(|| format!("cannot resolve the output path {}", output.display()))?;
        if resolved == input {
            return Ok(true);
        }
        // `canonicalize` does not collapse hard links: two names for one inode
        // stay different paths. Compare the inode on Unix as well.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a = fs::metadata(&input).context("cannot stat the input video")?;
            let b = fs::metadata(output)
                .with_context(|| format!("cannot stat {}", output.display()))?;
            if a.dev() == b.dev() && a.ino() == b.ino() {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    // The target does not exist yet. Resolve a symlinked parent and the `..`
    // spelling so a path that *would* be the input is still caught.
    let parent = match output.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let Some(name) = output.file_name() else {
        return Ok(false);
    };
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("cannot resolve the output folder {}", parent.display()))?;
    Ok(parent.join(name) == input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("clip.mp4");
        fs::write(&input, b"input bytes").unwrap();
        (dir, input)
    }

    #[test]
    fn default_name_lives_beside_the_input() {
        let (dir, input) = fixture();
        let out = default_output(&input);
        assert_eq!(out, dir.path().join("clip_textlayr.mp4"));
    }

    #[test]
    fn a_missing_extension_becomes_mp4() {
        let (dir, input) = fixture();
        let target = ExportTarget::resolve(&input, Some(&dir.path().join("out")), false).unwrap();
        assert_eq!(target.final_path(), dir.path().join("out.mp4"));
        assert_eq!(target.part_path(), dir.path().join(".out.mp4.part.mp4"));
    }

    #[test]
    fn mp4_is_accepted_in_any_case() {
        let (dir, input) = fixture();
        let target =
            ExportTarget::resolve(&input, Some(&dir.path().join("OUT.MP4")), false).unwrap();
        assert_eq!(target.final_path(), dir.path().join("OUT.MP4"));
    }

    #[test]
    fn a_dialog_that_appends_a_second_mp4_is_collapsed() {
        // GTK's portal save dialog turns the suggested `clip_textlayr.mp4` into
        // `clip_textlayr.mp4.mp4` (seen on Ubuntu 26.04).
        let (dir, input) = fixture();
        let target = ExportTarget::resolve(
            &input,
            Some(&dir.path().join("clip_textlayr.mp4.mp4")),
            false,
        )
        .unwrap();
        assert_eq!(target.final_path(), dir.path().join("clip_textlayr.mp4"));
        assert_eq!(
            target.part_path(),
            dir.path().join(".clip_textlayr.mp4.part.mp4")
        );
    }

    #[test]
    fn other_containers_are_rejected() {
        let (dir, input) = fixture();
        let err = ExportTarget::resolve(&input, Some(&dir.path().join("out.mov")), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains(".mp4"), "{err}");
    }

    #[test]
    fn the_part_file_is_hidden_and_ends_in_mp4() {
        let (dir, input) = fixture();
        let target =
            ExportTarget::resolve(&input, Some(&dir.path().join("out.mp4")), false).unwrap();
        let part = target.part_path().file_name().unwrap().to_string_lossy();
        assert_eq!(part, ".out.mp4.part.mp4");
    }

    #[test]
    fn an_existing_file_needs_confirmation() {
        let (dir, input) = fixture();
        let out = dir.path().join("out.mp4");
        fs::write(&out, b"old").unwrap();
        assert!(ExportTarget::resolve(&input, Some(&out), false).is_err());
        let target = ExportTarget::resolve(&input, Some(&out), true).unwrap();
        assert_eq!(target.final_path(), out);
    }

    #[test]
    fn the_default_name_needs_confirmation_too() {
        let (dir, input) = fixture();
        let default = dir.path().join("clip_textlayr.mp4");
        fs::write(&default, b"old").unwrap();
        assert!(ExportTarget::resolve(&input, None, false).is_err());
        let target = ExportTarget::resolve(&input, None, true).unwrap();
        assert_eq!(target.final_path(), default);
    }

    #[test]
    fn the_input_cannot_be_the_output() {
        let (dir, input) = fixture();
        let err = ExportTarget::resolve(&input, Some(&input), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("input video"), "{err}");

        // Different spelling, same file.
        let spelled = dir.path().join("sub").join("..").join("clip.mp4");
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        assert!(ExportTarget::resolve(&input, Some(&spelled), true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_link_to_the_input_is_rejected() {
        let (dir, input) = fixture();
        let link = dir.path().join("hard-link.mp4");
        fs::hard_link(&input, &link).unwrap();
        let err = ExportTarget::resolve(&input, Some(&link), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("input video"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_the_input_is_rejected() {
        let (dir, input) = fixture();
        let link = dir.path().join("symlink.mp4");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        assert!(ExportTarget::resolve(&input, Some(&link), true).is_err());
    }

    #[test]
    fn a_folder_or_missing_parent_is_rejected() {
        let (dir, input) = fixture();
        let folder = dir.path().join("folder.mp4");
        fs::create_dir_all(&folder).unwrap();
        assert!(ExportTarget::resolve(&input, Some(&folder), true).is_err());

        let missing = dir.path().join("missing").join("out.mp4");
        assert!(ExportTarget::resolve(&input, Some(&missing), false).is_err());
    }

    #[test]
    fn commit_replaces_the_target_and_discard_does_not() {
        let (dir, input) = fixture();

        let good = dir.path().join("out.mp4");
        fs::write(&good, b"old").unwrap();
        let target = ExportTarget::resolve(&input, Some(&good), true).unwrap();
        target.prepare().unwrap();
        fs::write(target.part_path(), b"new").unwrap();
        target.commit().unwrap();
        assert_eq!(fs::read(&good).unwrap(), b"new");
        assert!(!target.part_path().exists());

        let kept = dir.path().join("kept.mp4");
        fs::write(&kept, b"original").unwrap();
        let target = ExportTarget::resolve(&input, Some(&kept), true).unwrap();
        target.prepare().unwrap();
        fs::write(target.part_path(), b"partial").unwrap();
        target.discard();
        assert_eq!(fs::read(&kept).unwrap(), b"original");
        assert!(!target.part_path().exists());
    }
}
