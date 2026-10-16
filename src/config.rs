//! Style preferences for caption rendering, plus on-disk persistence.
//!
//! The [`Style`] struct is the single source of truth for how captions look.
//! It is persisted as JSON in the platform config dir (Milestone 4):
//! `~/Library/Application Support/textlayr/config.json` on macOS,
//! `~/.config/textlayr/config.json` on Linux. The CLI keeps the built-in
//! defaults and never reads this file, so headless runs stay deterministic.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::model::Model;
use crate::source::Source;

/// Straight (non-premultiplied) RGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
}

/// Everything that controls the look of the burned-in captions.
///
/// Percentage fields are relative to the video frame, so a style tuned on the
/// 1080x1920 sample scales to any resolution/orientation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    /// One of the bundled families: "Libre Baskerville", "Inter", "Nunito".
    pub font_family: String,
    /// Glyph em size, as a percentage of frame height.
    pub font_size_pct: f32,
    /// Color of the word currently being spoken.
    pub highlight: Color,
    /// Color of the surrounding (not-yet-spoken / already-spoken) words.
    pub dim: Color,
    /// Caption box fill color; alpha comes from `box_opacity`.
    pub box_color: Color,
    /// Box opacity, 0.0..=1.0.
    pub box_opacity: f32,
    pub rounded: bool,
    /// Corner radius as a fraction of box height (ignored when `rounded`).
    pub corner_radius_frac: f32,
    /// Words shown in one popup at a time (2..=6, clamped by the chunker).
    pub words_per_popup: usize,
    /// Vertical center of the box, as a percentage of frame height.
    pub position_pct: f32,
    /// Draw a soft drop shadow under the box.
    pub shadow: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            // The serif default matches the reference look TextLayr was built for:
            // a small rounded box with serif-ish caption text.
            font_family: "Libre Baskerville".to_string(),
            font_size_pct: 3.4,
            highlight: Color::new(255, 255, 255, 255),
            dim: Color::new(154, 154, 154, 255),
            box_color: Color::new(0, 0, 0, 255),
            box_opacity: 0.72,
            rounded: true,
            corner_radius_frac: 0.18,
            words_per_popup: 3,
            position_pct: 72.0,
            shadow: true,
        }
    }
}

/// Bundled font families, in menu order.
pub const FONT_FAMILIES: [&str; 3] = ["Libre Baskerville", "Inter", "Nunito"];

impl Style {
    /// Clamp fields into sane ranges (defensive against hand-edited configs).
    pub fn sanitize(&mut self) {
        self.font_size_pct = self.font_size_pct.clamp(1.0, 20.0);
        self.box_opacity = self.box_opacity.clamp(0.0, 1.0);
        self.corner_radius_frac = self.corner_radius_frac.clamp(0.0, 0.5);
        self.words_per_popup = self.words_per_popup.clamp(2, 6);
        self.position_pct = self.position_pct.clamp(5.0, 95.0);
        if !FONT_FAMILIES.contains(&self.font_family.as_str()) {
            self.font_family = FONT_FAMILIES[0].to_string();
        }
    }
}

/// Locate the bundled `assets/fonts` directory, and where it came from.
///
/// Order: `$TEXTLAYR_FONTS` → bundled `Contents/Resources/assets/fonts` →
/// `assets/fonts` next to / above the executable (dev) → under the current
/// directory (dev).
pub fn find_fonts_dir() -> Result<(PathBuf, Source)> {
    let mut candidates: Vec<(PathBuf, Source)> = Vec::new();

    if let Ok(env) = std::env::var("TEXTLAYR_FONTS") {
        candidates.push((PathBuf::from(env), Source::Env));
    }

    if let Ok(exe) = std::env::current_exe() {
        // .app bundle: the exe lives in Contents/MacOS, fonts are copied to
        // Contents/Resources/assets/fonts by scripts/package_macos.sh.
        if let Some(dir) = exe.parent() {
            if let Some(res) = dir.parent() {
                candidates.push((
                    res.join("Resources").join("assets").join("fonts"),
                    Source::Bundled,
                ));
            }
        }
        let mut cur = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            candidates.push((dir.join("assets").join("fonts"), Source::Dev));
            cur = dir.parent().map(|p| p.to_path_buf());
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd);
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            candidates.push((dir.join("assets").join("fonts"), Source::Dev));
            cur = dir.parent().map(|p| p.to_path_buf());
        }
    }

    for (c, source) in &candidates {
        if c.is_dir() && c.join("LibreBaskerville-Regular.ttf").is_file() {
            return Ok((c.clone(), *source));
        }
    }

    bail!(
        "bundled fonts not found (looked for assets/fonts/LibreBaskerville-Regular.ttf). \
         Set $TEXTLAYR_FONTS to the directory holding the caption fonts."
    )
}

/// Map a family name ("Libre Baskerville") to its on-disk key ("LibreBaskerville").
pub fn font_key(family: &str) -> String {
    family.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Resolve a family to its (regular, bold) TTF paths inside `fonts_dir`.
pub fn font_paths(fonts_dir: &Path, family: &str) -> Result<(PathBuf, PathBuf)> {
    let key = font_key(family);
    let regular = fonts_dir.join(format!("{key}-Regular.ttf"));
    let bold = fonts_dir.join(format!("{key}-Bold.ttf"));
    if !regular.is_file() {
        bail!("missing font file: {}", regular.display());
    }
    if !bold.is_file() {
        bail!("missing font file: {}", bold.display());
    }
    Ok((regular, bold))
}

/// Read a font file into memory with a helpful error.
pub fn load_font_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read font {}", path.display()))
}

// ---------------------------------------------------------------------------
// Persistence (Milestone 4)
// ---------------------------------------------------------------------------

/// Bump when the on-disk shape changes incompatibly.
pub const CONFIG_VERSION: u32 = 1;

/// The persisted document. Wrapped (with a version) so future app preferences
/// can be added without breaking old files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    version: u32,
    style: Style,
    /// Preferred whisper model (id form).
    model: Model,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            style: Style::default(),
            model: Model::default(),
        }
    }
}

/// Everything that persists between runs: the caption style and the chosen
/// whisper model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Prefs {
    pub style: Style,
    pub model: Model,
}

/// `<config dir>/textlayr/config.json` (`None` if the platform has no config dir).
///
/// macOS: `~/Library/Application Support/textlayr/config.json`.
/// Linux: `~/.config/textlayr/config.json`.
pub fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("textlayr").join("config.json"))
}

/// Load persisted preferences, falling back to defaults when the file is
/// absent, unreadable, or invalid. Never fails: a corrupt config must not stop
/// the app from starting.
pub fn load_prefs() -> Prefs {
    let path = match config_path() {
        Some(p) => p,
        None => return Prefs::default(),
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Prefs::default();
    };
    match serde_json::from_slice::<Config>(&bytes) {
        Ok(cfg) => {
            let mut style = cfg.style;
            style.sanitize();
            Prefs {
                style,
                model: cfg.model,
            }
        }
        Err(e) => {
            eprintln!("(ignoring unreadable config {}: {e})", path.display());
            Prefs::default()
        }
    }
}

/// Persist preferences atomically (write a temp file, then rename) so an
/// interrupted write can never leave a half-written config behind.
pub fn save_prefs(prefs: &Prefs) -> Result<()> {
    let path = config_path().context("no platform config directory")?;
    save_prefs_to(&path, prefs)
}

/// [`save_prefs`] to an explicit path (also used by tests).
pub fn save_prefs_to(path: &Path, prefs: &Prefs) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let cfg = Config {
        version: CONFIG_VERSION,
        style: prefs.style.clone(),
        model: prefs.model,
    };
    let json = serde_json::to_vec_pretty(&cfg).context("serialize config")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_round_trips_through_json() {
        let style = Style {
            font_family: "Nunito".to_string(),
            font_size_pct: 5.5,
            highlight: Color::new(1, 2, 3, 4),
            words_per_popup: 5,
            ..Style::default()
        };
        let json = serde_json::to_vec(&Config {
            version: CONFIG_VERSION,
            style: style.clone(),
            model: Model::SmallEn,
        })
        .unwrap();
        let back: Config = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.style, style);
        assert_eq!(back.version, CONFIG_VERSION);
        assert_eq!(back.model, Model::SmallEn);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // A hand-written partial file still parses; unspecified fields default.
        let cfg: Config = serde_json::from_str(r#"{"style":{"font_size_pct":9.0}}"#).unwrap();
        assert_eq!(cfg.style.font_size_pct, 9.0);
        assert_eq!(cfg.style.font_family, Style::default().font_family);
        assert_eq!(cfg.model, Model::default());
    }

    #[test]
    fn save_then_load_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("config.json");
        let prefs = Prefs {
            style: Style {
                position_pct: 33.0,
                dim: Color::new(9, 8, 7, 255),
                ..Style::default()
            },
            model: Model::SmallEn,
        };
        save_prefs_to(&path, &prefs).unwrap();
        assert!(path.is_file());
        // The temp file used during the atomic write is gone.
        assert!(!path.with_extension("json.tmp").exists());

        let bytes = std::fs::read(&path).unwrap();
        let cfg: Config = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(cfg.style.position_pct, 33.0);
        assert_eq!(cfg.style.dim, Color::new(9, 8, 7, 255));
        assert_eq!(cfg.model, Model::SmallEn);
    }

    #[test]
    fn out_of_range_values_are_sanitized() {
        let cfg: Config = serde_json::from_str(
            r#"{"style":{"words_per_popup":99,"font_size_pct":1000.0,"font_family":"Comic Sans"}}"#,
        )
        .unwrap();
        let mut style = cfg.style;
        style.sanitize();
        assert_eq!(style.words_per_popup, 6);
        assert_eq!(style.font_size_pct, 20.0);
        assert_eq!(style.font_family, FONT_FAMILIES[0]);
    }
}
