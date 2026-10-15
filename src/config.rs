//! Style preferences for caption rendering.
//!
//! The `Style` struct is the single source of truth for how captions look.
//! Persistence (serde JSON in the platform config dir) lands in Milestone 4;
//! for now it carries `Serialize`/`Deserialize` so that lands without rework.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
            // Serif matches the reference screenshot (samples/text example
            // screen shot.png).
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

/// Locate the bundled `assets/fonts` directory.
///
/// Order: `$TEXXY_FONTS` → `assets/fonts` next to / above the executable
/// (handles `target/debug`) → `assets/fonts` under the current directory.
pub fn find_fonts_dir() -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(env) = std::env::var("TEXXY_FONTS") {
        candidates.push(PathBuf::from(env));
    }

    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            candidates.push(dir.join("assets").join("fonts"));
            cur = dir.parent().map(|p| p.to_path_buf());
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd);
        for _ in 0..6 {
            let Some(dir) = cur else { break };
            candidates.push(dir.join("assets").join("fonts"));
            cur = dir.parent().map(|p| p.to_path_buf());
        }
    }

    for c in &candidates {
        if c.is_dir() && c.join("LibreBaskerville-Regular.ttf").is_file() {
            return Ok(c.clone());
        }
    }

    bail!(
        "bundled fonts not found (looked for assets/fonts/LibreBaskerville-Regular.ttf). \
         Set $TEXXY_FONTS to the directory holding the caption fonts."
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
