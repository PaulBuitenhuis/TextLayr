//! Style preferences for caption rendering, plus on-disk persistence.
//!
//! The [`Style`] struct is the single source of truth for how captions look.
//! It is persisted as JSON in the platform config dir (Milestone 4):
//! `~/Library/Application Support/textlayr/config.json` on macOS,
//! `~/.config/textlayr/config.json` on Linux. The CLI keeps the built-in
//! defaults and never reads this file, so headless runs stay deterministic.
//!
//! The on-disk shape is versioned. Version 3 groups the flat v2 style into
//! `font`, `text`, `box` and `layout` sub-objects; every group is
//! `#[serde(default)]`, so new fields can be added without breaking old
//! configs or profiles. Version 2 files are migrated on load.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::model::Model;
use crate::platform;
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
/// Percentage and em fields are relative to the video frame or the font size,
/// so a style tuned on the 1080x1920 sample scales to any resolution and
/// orientation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub font: FontStyle,
    pub text: TextStyle,
    #[serde(rename = "box")]
    pub box_style: BoxStyle,
    pub layout: LayoutStyle,
    pub highlight: HighlightStyle,
    pub motion: MotionStyle,
}

/// Typeface choice and glyph size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontStyle {
    /// One of the bundled families: "Libre Baskerville", "Inter", "Nunito".
    pub family: String,
    /// Glyph em size, as a percentage of frame height.
    pub size_pct: f32,
    /// Draw the spoken word with the bold face (the default).
    pub active_bold: bool,
    /// Draw the surrounding words with the bold face.
    pub dim_bold: bool,
    /// Render caption text in capitals.
    pub uppercase: bool,
    /// Extra tracking between characters, as a fraction of the font size.
    pub letter_spacing: f32,
}

impl Default for FontStyle {
    fn default() -> Self {
        FontStyle {
            // The serif default matches the reference look TextLayr was built
            // for: a small rounded box with serif-ish caption text.
            family: "Libre Baskerville".to_string(),
            size_pct: 3.4,
            active_bold: true,
            dim_bold: false,
            uppercase: false,
            letter_spacing: 0.0,
        }
    }
}

/// Colors of the words themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    /// Color of the word currently being spoken.
    pub highlight: Color,
    /// Color of the surrounding (not-yet-spoken / already-spoken) words.
    pub dim: Color,
    /// Multiplier on the text's alpha (0.0..=1.0).
    pub opacity: f32,
    pub outline: OutlineStyle,
    pub shadow: TextShadowStyle,
    pub glow: GlowStyle,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            highlight: Color::new(255, 255, 255, 255),
            dim: Color::new(154, 154, 154, 255),
            opacity: 1.0,
            outline: OutlineStyle::default(),
            shadow: TextShadowStyle::default(),
            glow: GlowStyle::default(),
        }
    }
}

/// A solid stroke around the glyphs, drawn behind the fill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutlineStyle {
    pub enabled: bool,
    pub color: Color,
    /// Stroke width as a fraction of the font size.
    pub width: f32,
}

impl Default for OutlineStyle {
    fn default() -> Self {
        OutlineStyle {
            enabled: false,
            color: Color::new(0, 0, 0, 255),
            width: 0.08,
        }
    }
}

/// A soft offset shadow under the glyphs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextShadowStyle {
    pub enabled: bool,
    pub color: Color,
    /// Offset as a fraction of the font size (may be negative).
    pub offset_x: f32,
    pub offset_y: f32,
    /// Blur radius as a fraction of the font size.
    pub blur: f32,
}

impl Default for TextShadowStyle {
    fn default() -> Self {
        TextShadowStyle {
            enabled: false,
            color: Color::new(0, 0, 0, 255),
            offset_x: 0.06,
            offset_y: 0.06,
            blur: 0.04,
        }
    }
}

/// A soft coloured halo around the glyphs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlowStyle {
    pub enabled: bool,
    pub color: Color,
    /// Halo radius as a fraction of the font size.
    pub radius: f32,
}

impl Default for GlowStyle {
    fn default() -> Self {
        GlowStyle {
            enabled: false,
            color: Color::new(255, 214, 102, 180),
            radius: 0.10,
        }
    }
}

/// The rounded rectangle behind the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BoxStyle {
    /// Caption box fill color; alpha comes from `opacity`.
    pub color: Color,
    /// Box opacity, 0.0..=1.0.
    pub opacity: f32,
    pub rounded: bool,
    /// Corner radius as a fraction of box height (ignored when `rounded`).
    pub radius_frac: f32,
    /// Horizontal padding inside the box, as a fraction of the font size.
    pub padding_x: f32,
    /// Vertical padding inside the box, as a fraction of the font size.
    pub padding_y: f32,
    /// Stroke width around the box, as a fraction of the font size.
    pub border_width: f32,
    pub border_color: Color,
    /// Draw a soft drop shadow under the box.
    pub shadow: bool,
    pub shadow_color: Color,
    /// Multiplier on the shadow's alpha (0.0..=1.0).
    pub shadow_opacity: f32,
    /// Shadow offset as a fraction of the font size (may be negative).
    pub shadow_offset_y: f32,
    /// Shadow blur as a fraction of the font size.
    pub shadow_blur: f32,
}

impl Default for BoxStyle {
    fn default() -> Self {
        BoxStyle {
            color: Color::new(0, 0, 0, 255),
            opacity: 0.72,
            rounded: true,
            radius_frac: 0.18,
            padding_x: 0.50,
            padding_y: 0.24,
            border_width: 0.0,
            border_color: Color::new(255, 255, 255, 255),
            shadow: true,
            shadow_color: Color::new(0, 0, 0, 255),
            shadow_opacity: 1.0,
            shadow_offset_y: 0.10,
            shadow_blur: 0.14,
        }
    }
}

/// Treatment applied to the word currently being spoken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HighlightStyle {
    /// Scale multiplier for the active word (1.0 = no change).
    pub scale: f32,
    pub pill: PillStyle,
    pub underline: UnderlineStyle,
}

impl Default for HighlightStyle {
    fn default() -> Self {
        HighlightStyle {
            scale: 1.0,
            pill: PillStyle::default(),
            underline: UnderlineStyle::default(),
        }
    }
}

/// A rounded plate behind the active word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PillStyle {
    pub enabled: bool,
    pub color: Color,
    /// Horizontal padding, as a fraction of the font size.
    pub padding_x: f32,
    /// Vertical padding, as a fraction of the font size.
    pub padding_y: f32,
    /// Corner radius as a fraction of the pill height.
    pub radius_frac: f32,
}

impl Default for PillStyle {
    fn default() -> Self {
        PillStyle {
            enabled: false,
            color: Color::new(255, 209, 102, 255),
            padding_x: 0.12,
            padding_y: 0.04,
            radius_frac: 0.30,
        }
    }
}

/// A line under the active word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnderlineStyle {
    pub enabled: bool,
    pub color: Color,
    /// Line thickness as a fraction of the font size.
    pub width: f32,
    /// Gap below the descender, as a fraction of the font size.
    pub offset: f32,
}

impl Default for UnderlineStyle {
    fn default() -> Self {
        UnderlineStyle {
            enabled: false,
            color: Color::new(255, 255, 255, 255),
            width: 0.05,
            offset: 0.02,
        }
    }
}

/// Which edge (or centre) of the caption box `position_pct` refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    #[default]
    Center,
    Top,
    Bottom,
}

/// Horizontal alignment of lines inside the caption box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Center,
    Left,
    Right,
}

/// What happens when the caption text is wider than the available space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// One line, shrinking the font until it fits (the original behaviour).
    #[default]
    Shrink,
    /// Keep the font size, wrap onto up to `max_lines` lines. Shrinks only as
    /// a last resort when the words cannot be made to fit at all.
    Wrap,
    /// Keep the font size and clip anything that does not fit.
    Clip,
}

impl<'de> Deserialize<'de> for Anchor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "top" => Anchor::Top,
            "bottom" => Anchor::Bottom,
            _ => Anchor::Center,
        })
    }
}

impl<'de> Deserialize<'de> for Align {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "left" => Align::Left,
            "right" => Align::Right,
            _ => Align::Center,
        })
    }
}

impl<'de> Deserialize<'de> for Fit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "wrap" => Fit::Wrap,
            "clip" => Fit::Clip,
            _ => Fit::Shrink,
        })
    }
}

/// Where the caption box sits in the frame, and how its lines are laid out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutStyle {
    /// Vertical position of the box, as a percentage of frame height. What it
    /// refers to depends on `anchor`.
    pub position_pct: f32,
    pub anchor: Anchor,
    pub align: Align,
    pub fit: Fit,
    /// Hard cap on wrapped lines (1..=4).
    pub max_lines: usize,
    /// Maximum words per line when wrapping; 0 means width-driven only.
    pub max_words_per_line: usize,
    /// Baseline-to-baseline distance, as a multiple of the font size.
    pub line_height: f32,
    /// Reserve this many lines of box height even when fewer are used; 0 means
    /// natural height.
    pub reserve_lines: usize,
    /// Maximum text width as a percentage of frame width, when the box is not
    /// fixed-width.
    pub max_width_pct: f32,
    /// Fixed box width as a percentage of frame width; `None` hugs the text.
    pub box_width_pct: Option<f32>,
    /// Fixed box height as a percentage of frame height; `None` derives it
    /// from the text and `reserve_lines`.
    pub box_height_pct: Option<f32>,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        LayoutStyle {
            position_pct: 72.0,
            anchor: Anchor::Center,
            align: Align::Center,
            fit: Fit::Shrink,
            max_lines: 1,
            max_words_per_line: 0,
            line_height: 1.2,
            reserve_lines: 0,
            max_width_pct: 92.0,
            box_width_pct: None,
            box_height_pct: None,
        }
    }
}

/// How transcribed words are grouped into popups. Kept separate from [`Style`]
/// because changing it re-chunks the transcript and can discard edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grouping {
    /// Words shown in one popup at a time (1..=8).
    pub words_per_popup: usize,
}

impl Default for Grouping {
    fn default() -> Self {
        Grouping { words_per_popup: 3 }
    }
}

/// Bundled font families, in menu order.
pub const FONT_FAMILIES: [&str; 3] = ["Libre Baskerville", "Inter", "Nunito"];

impl Style {
    /// Clamp fields into sane ranges (defensive against hand-edited configs
    /// and profiles saved by a newer version).
    pub fn sanitize(&mut self) {
        self.font.size_pct = self.font.size_pct.clamp(1.0, 20.0);
        self.font.letter_spacing = self.font.letter_spacing.clamp(0.0, 0.3);
        if !FONT_FAMILIES.contains(&self.font.family.as_str()) {
            self.font.family = FONT_FAMILIES[0].to_string();
        }
        self.text.opacity = self.text.opacity.clamp(0.0, 1.0);
        self.text.outline.width = self.text.outline.width.clamp(0.0, 0.3);
        self.text.shadow.offset_x = self.text.shadow.offset_x.clamp(-0.5, 0.5);
        self.text.shadow.offset_y = self.text.shadow.offset_y.clamp(-0.5, 0.5);
        self.text.shadow.blur = self.text.shadow.blur.clamp(0.0, 0.5);
        self.text.glow.radius = self.text.glow.radius.clamp(0.0, 0.5);
        self.box_style.opacity = self.box_style.opacity.clamp(0.0, 1.0);
        self.box_style.radius_frac = self.box_style.radius_frac.clamp(0.0, 0.5);
        self.box_style.padding_x = self.box_style.padding_x.clamp(0.0, 2.0);
        self.box_style.padding_y = self.box_style.padding_y.clamp(0.0, 1.5);
        self.box_style.border_width = self.box_style.border_width.clamp(0.0, 0.3);
        self.box_style.shadow_opacity = self.box_style.shadow_opacity.clamp(0.0, 1.0);
        self.box_style.shadow_offset_y = self.box_style.shadow_offset_y.clamp(-0.5, 0.5);
        self.box_style.shadow_blur = self.box_style.shadow_blur.clamp(0.0, 0.5);
        self.highlight.scale = self.highlight.scale.clamp(1.0, 1.4);
        self.highlight.pill.padding_x = self.highlight.pill.padding_x.clamp(0.0, 1.0);
        self.highlight.pill.padding_y = self.highlight.pill.padding_y.clamp(0.0, 1.0);
        self.highlight.pill.radius_frac = self.highlight.pill.radius_frac.clamp(0.0, 1.0);
        self.highlight.underline.width = self.highlight.underline.width.clamp(0.0, 0.3);
        self.highlight.underline.offset = self.highlight.underline.offset.clamp(0.0, 0.3);
        self.layout.position_pct = self.layout.position_pct.clamp(5.0, 95.0);
        self.layout.max_lines = self.layout.max_lines.clamp(1, 4);
        self.layout.max_words_per_line = self.layout.max_words_per_line.clamp(0, 8);
        self.layout.line_height = self.layout.line_height.clamp(1.0, 2.0);
        self.layout.reserve_lines = self.layout.reserve_lines.clamp(0, 4);
        self.layout.max_width_pct = self.layout.max_width_pct.clamp(40.0, 100.0);
        if let Some(w) = &mut self.layout.box_width_pct {
            *w = w.clamp(10.0, 100.0);
        }
        if let Some(h) = &mut self.layout.box_height_pct {
            *h = h.clamp(5.0, 60.0);
        }
        self.motion.duration_ms = self.motion.duration_ms.clamp(0, 1200);
        self.motion.exit_duration_ms = self.motion.exit_duration_ms.clamp(0, 1200);
        self.motion.intensity = self.motion.intensity.clamp(0.0, 1.5);
    }
}

impl Grouping {
    pub fn sanitize(&mut self) {
        self.words_per_popup = self.words_per_popup.clamp(1, 8);
    }
}

/// How a whole caption popup enters the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PopupEffect {
    #[default]
    None,
    Pop,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
    Fade,
}

impl<'de> Deserialize<'de> for PopupEffect {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "pop" => PopupEffect::Pop,
            "slide_up" => PopupEffect::SlideUp,
            "slide_down" => PopupEffect::SlideDown,
            "slide_left" => PopupEffect::SlideLeft,
            "slide_right" => PopupEffect::SlideRight,
            "fade" => PopupEffect::Fade,
            _ => PopupEffect::None,
        })
    }
}

/// Timing curve for a popup transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    Linear,
    #[default]
    EaseOut,
    EaseInOut,
    /// Overshoots slightly before settling, for a springy pop.
    Spring,
}

impl<'de> Deserialize<'de> for Easing {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "linear" => Easing::Linear,
            "ease_in_out" => Easing::EaseInOut,
            "spring" => Easing::Spring,
            _ => Easing::EaseOut,
        })
    }
}

/// Motion applied when a new caption popup appears.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MotionStyle {
    /// Entrance effect when a popup appears.
    pub popup: PopupEffect,
    /// Exit effect when a popup disappears.
    pub exit: PopupEffect,
    /// Entrance length in milliseconds (0 disables it).
    pub duration_ms: u32,
    /// Exit length in milliseconds (0 disables it).
    pub exit_duration_ms: u32,
    pub easing: Easing,
    /// Scale delta for Pop, or distance in em for slide effects.
    pub intensity: f32,
}

impl Default for MotionStyle {
    fn default() -> Self {
        MotionStyle {
            popup: PopupEffect::None,
            exit: PopupEffect::None,
            duration_ms: 260,
            exit_duration_ms: 160,
            easing: Easing::EaseOut,
            intensity: 0.20,
        }
    }
}

/// Locate the bundled `assets/fonts` directory, and where it came from.
///
/// Order: `$TEXTLAYR_FONTS` → the app's bundled resource root(s) → a source
/// checkout, walking up from the executable and the current directory.
pub fn find_fonts_dir() -> Result<(PathBuf, Source)> {
    let mut candidates: Vec<(PathBuf, Source)> = Vec::new();

    if let Ok(env) = std::env::var("TEXTLAYR_FONTS") {
        candidates.push((PathBuf::from(env), Source::Env));
    }

    let fonts = |root: PathBuf| root.join("assets").join("fonts");
    for (root, source) in platform::bundle_roots() {
        candidates.push((fonts(root), source));
    }
    for (root, source) in platform::dev_roots() {
        candidates.push((fonts(root), source));
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
// Persistence
// ---------------------------------------------------------------------------

/// Bump when the on-disk shape changes incompatibly.
pub const CONFIG_VERSION: u32 = 3;

/// The persisted document. Wrapped (with a version) so future app preferences
/// can be added without breaking old files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    version: u32,
    style: Style,
    grouping: Grouping,
    /// Stable id of the style profile last applied, if any.
    active_profile: Option<String>,
    /// Preferred whisper model (id form).
    model: Model,
    /// Whether to check for a newer TextLayr release on launch.
    auto_check_updates: bool,
    /// Unix seconds of the last update check (success or failure), used to
    /// throttle the automatic check to once a day.
    last_update_check: Option<u64>,
    /// Folder of the last export destination chosen in the save dialog.
    last_export_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            style: Style::default(),
            grouping: Grouping::default(),
            active_profile: None,
            model: Model::default(),
            auto_check_updates: true,
            last_update_check: None,
            last_export_dir: None,
        }
    }
}

/// The v2 on-disk shape: one flat `style` object with `words_per_popup` in it.
/// Only used to migrate existing configs; never written.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct ConfigV2 {
    #[allow(dead_code)]
    version: u32,
    style: StyleV2,
    model: Model,
    auto_check_updates: bool,
    last_update_check: Option<u64>,
}

impl Default for ConfigV2 {
    fn default() -> Self {
        ConfigV2 {
            version: 2,
            style: StyleV2::default(),
            model: Model::default(),
            auto_check_updates: true,
            last_update_check: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct StyleV2 {
    font_family: String,
    font_size_pct: f32,
    highlight: Color,
    dim: Color,
    box_color: Color,
    box_opacity: f32,
    rounded: bool,
    corner_radius_frac: f32,
    words_per_popup: usize,
    position_pct: f32,
    shadow: bool,
}

impl Default for StyleV2 {
    fn default() -> Self {
        let s = Style::default();
        StyleV2 {
            font_family: s.font.family,
            font_size_pct: s.font.size_pct,
            highlight: s.text.highlight,
            dim: s.text.dim,
            box_color: s.box_style.color,
            box_opacity: s.box_style.opacity,
            rounded: s.box_style.rounded,
            corner_radius_frac: s.box_style.radius_frac,
            words_per_popup: Grouping::default().words_per_popup,
            position_pct: s.layout.position_pct,
            shadow: s.box_style.shadow,
        }
    }
}

impl From<StyleV2> for (Style, Grouping) {
    fn from(s: StyleV2) -> Self {
        (
            Style {
                font: FontStyle {
                    family: s.font_family,
                    size_pct: s.font_size_pct,
                    ..FontStyle::default()
                },
                text: TextStyle {
                    highlight: s.highlight,
                    dim: s.dim,
                    ..TextStyle::default()
                },
                box_style: BoxStyle {
                    color: s.box_color,
                    opacity: s.box_opacity,
                    rounded: s.rounded,
                    radius_frac: s.corner_radius_frac,
                    shadow: s.shadow,
                    ..BoxStyle::default()
                },
                layout: LayoutStyle {
                    position_pct: s.position_pct,
                    ..LayoutStyle::default()
                },
                highlight: HighlightStyle::default(),
                motion: MotionStyle::default(),
            },
            Grouping {
                words_per_popup: s.words_per_popup,
            },
        )
    }
}

/// Everything that persists between runs: the caption style, grouping, the
/// active profile, the chosen whisper model, and the update preferences.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub style: Style,
    pub grouping: Grouping,
    pub active_profile: Option<String>,
    pub model: Model,
    pub auto_check_updates: bool,
    pub last_update_check: Option<u64>,
    /// Folder of the last export destination, if one has been chosen.
    pub last_export_dir: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            style: Style::default(),
            grouping: Grouping::default(),
            active_profile: None,
            model: Model::default(),
            // Checking on launch is on by default; the user can turn it off.
            auto_check_updates: true,
            last_update_check: None,
            last_export_dir: None,
        }
    }
}

/// `<config dir>/textlayr` (`None` if the platform has no config dir).
///
/// macOS: `~/Library/Application Support/textlayr`.
/// Linux: `~/.config/textlayr`.
pub fn config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("textlayr"))
}

/// `<config dir>/textlayr/config.json`.
pub fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.json"))
}

/// Load persisted preferences, falling back to defaults when the file is
/// absent, unreadable, or invalid. Never fails: a corrupt config must not stop
/// the app from starting.
pub fn load_prefs() -> Prefs {
    let Some(path) = config_path() else {
        return Prefs::default();
    };
    let (prefs, migrated) = load_prefs_inner(&path);
    if migrated {
        // Rewrite a v2 file in the grouped v3 shape once, so later runs don't
        // have to migrate and new fields have somewhere to live.
        if let Err(e) = save_prefs_to(&path, &prefs) {
            eprintln!(
                "(could not rewrite migrated config {}: {e:#})",
                path.display()
            );
        }
    }
    prefs
}

/// [`load_prefs`] from an explicit path (also used by tests).
pub fn load_prefs_from(path: &Path) -> Prefs {
    load_prefs_inner(path).0
}

/// Returns the loaded preferences and whether the file was a legacy v2 shape
/// that should be rewritten.
fn load_prefs_inner(path: &Path) -> (Prefs, bool) {
    let Ok(bytes) = std::fs::read(path) else {
        return (Prefs::default(), false);
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("(ignoring unreadable config {}: {e})", path.display());
            return (Prefs::default(), false);
        }
    };

    // A v3 file says so in its version, and/or has grouped style keys. A v2
    // style is one flat object. Never fall through to v2 after deciding this is
    // v3: the flat parser would silently accept the v3 object with all default
    // values and lose the user's style.
    let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    let grouped_style = value
        .get("style")
        .and_then(|s| s.as_object())
        .map(|o| o.contains_key("font") || o.contains_key("text") || o.contains_key("box"))
        .unwrap_or(false);
    let looks_grouped = version >= CONFIG_VERSION as u64 || grouped_style;

    if looks_grouped {
        match serde_json::from_value::<Config>(value) {
            Ok(cfg) => {
                if cfg.version > CONFIG_VERSION {
                    eprintln!(
                        "(config {} was written by a newer TextLayr (v{}); loading best-effort)",
                        path.display(),
                        cfg.version
                    );
                }
                let mut style = cfg.style;
                let mut grouping = cfg.grouping;
                style.sanitize();
                grouping.sanitize();
                return (
                    Prefs {
                        style,
                        grouping,
                        active_profile: cfg.active_profile.filter(|id| !id.trim().is_empty()),
                        model: cfg.model,
                        auto_check_updates: cfg.auto_check_updates,
                        last_update_check: cfg.last_update_check,
                        last_export_dir: cfg.last_export_dir,
                    },
                    false,
                );
            }
            Err(e) => {
                eprintln!("(ignoring unreadable config {}: {e})", path.display());
                return (Prefs::default(), false);
            }
        }
    }

    match serde_json::from_value::<ConfigV2>(value) {
        Ok(cfg) => {
            let (mut style, mut grouping) = (cfg.style).into();
            style.sanitize();
            grouping.sanitize();
            (
                Prefs {
                    style,
                    grouping,
                    active_profile: None,
                    model: cfg.model,
                    auto_check_updates: cfg.auto_check_updates,
                    last_update_check: cfg.last_update_check,
                    last_export_dir: None,
                },
                true,
            )
        }
        Err(e) => {
            eprintln!("(ignoring unreadable config {}: {e})", path.display());
            (Prefs::default(), false)
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
        grouping: prefs.grouping.clone(),
        active_profile: prefs.active_profile.clone(),
        model: prefs.model,
        auto_check_updates: prefs.auto_check_updates,
        last_update_check: prefs.last_update_check,
        last_export_dir: prefs.last_export_dir.clone(),
    };
    let json = serde_json::to_vec_pretty(&cfg).context("serialize config")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
    // Keep the last good v3 document beside the live file. An older build
    // (which only understands v2) can otherwise overwrite config.json with its
    // flat default shape; the backup survives that and is recoverable.
    if path.is_file() {
        let backup = path.with_file_name("config-v3.bak");
        let _ = std::fs::copy(path, backup);
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grouped_style() -> Style {
        Style {
            font: FontStyle {
                family: "Nunito".to_string(),
                size_pct: 5.5,
                ..FontStyle::default()
            },
            text: TextStyle {
                highlight: Color::new(1, 2, 3, 4),
                ..TextStyle::default()
            },
            ..Style::default()
        }
    }

    #[test]
    fn style_round_trips_through_json() {
        let style = grouped_style();
        let json = serde_json::to_vec(&Config {
            style: style.clone(),
            grouping: Grouping { words_per_popup: 5 },
            active_profile: Some("builtin:bold-pop".into()),
            model: Model::SmallEn,
            auto_check_updates: false,
            last_update_check: Some(1_700_000_000),
            ..Config::default()
        })
        .unwrap();
        let back: Config = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.style, style);
        assert_eq!(back.version, CONFIG_VERSION);
        assert_eq!(back.grouping.words_per_popup, 5);
        assert_eq!(back.active_profile.as_deref(), Some("builtin:bold-pop"));
        assert_eq!(back.model, Model::SmallEn);
        assert!(!back.auto_check_updates);
        assert_eq!(back.last_update_check, Some(1_700_000_000));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // A hand-written partial v3 file still parses; unspecified groups and
        // fields default. This is what keeps old profiles working as style
        // fields are added.
        let cfg: Config = serde_json::from_str(r#"{"style":{"font":{"size_pct":9.0}}}"#).unwrap();
        assert_eq!(cfg.style.font.size_pct, 9.0);
        assert_eq!(cfg.style.font.family, Style::default().font.family);
        assert_eq!(cfg.style.box_style, BoxStyle::default());
        assert_eq!(cfg.grouping, Grouping::default());
        assert_eq!(cfg.model, Model::default());
        // Update checking is on by default even for a config that predates it.
        assert!(cfg.auto_check_updates);
        assert_eq!(cfg.last_update_check, None);
        assert_eq!(cfg.last_export_dir, None);
        assert_eq!(cfg.active_profile, None);
    }

    #[test]
    fn save_then_load_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("config.json");
        let prefs = Prefs {
            style: Style {
                layout: LayoutStyle {
                    position_pct: 33.0,
                    ..Default::default()
                },
                text: TextStyle {
                    dim: Color::new(9, 8, 7, 255),
                    ..TextStyle::default()
                },
                ..Style::default()
            },
            grouping: Grouping { words_per_popup: 6 },
            active_profile: Some("user:mine".into()),
            model: Model::SmallEn,
            auto_check_updates: false,
            last_update_check: Some(42),
            last_export_dir: Some(PathBuf::from("/tmp/exports")),
        };
        save_prefs_to(&path, &prefs).unwrap();
        assert!(path.is_file());
        // The temp file used during the atomic write is gone.
        assert!(!path.with_extension("json.tmp").exists());

        let (loaded, migrated) = load_prefs_inner(&path);
        assert!(!migrated);
        assert_eq!(loaded.style.layout.position_pct, 33.0);
        assert_eq!(loaded.style.text.dim, Color::new(9, 8, 7, 255));
        assert_eq!(loaded.grouping.words_per_popup, 6);
        assert_eq!(loaded.active_profile.as_deref(), Some("user:mine"));
        assert_eq!(loaded.model, Model::SmallEn);
        assert!(!loaded.auto_check_updates);
        assert_eq!(loaded.last_update_check, Some(42));
        assert_eq!(
            loaded.last_export_dir.as_deref(),
            Some(Path::new("/tmp/exports"))
        );
    }

    #[test]
    fn saving_keeps_the_previous_document_as_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut prefs = Prefs::default();
        prefs.style.font.family = "Inter".to_string();
        save_prefs_to(&path, &prefs).unwrap();

        prefs.style.font.family = "Nunito".to_string();
        save_prefs_to(&path, &prefs).unwrap();

        let current = load_prefs_from(&path);
        assert_eq!(current.style.font.family, "Nunito");
        let backup = load_prefs_from(&dir.path().join("config-v3.bak"));
        assert_eq!(backup.style.font.family, "Inter");
    }

    #[test]
    fn v2_config_migrates_into_grouped_style_and_grouping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                "version": 2,
                "style": {
                    "font_family": "Inter",
                    "font_size_pct": 4.2,
                    "highlight": {"r": 1, "g": 2, "b": 3, "a": 255},
                    "dim": {"r": 4, "g": 5, "b": 6, "a": 255},
                    "box_color": {"r": 7, "g": 8, "b": 9, "a": 255},
                    "box_opacity": 0.4,
                    "rounded": false,
                    "corner_radius_frac": 0.3,
                    "words_per_popup": 5,
                    "position_pct": 44.0,
                    "shadow": false
                },
                "model": "small.en",
                "auto_check_updates": false,
                "last_update_check": 99
            }"#,
        )
        .unwrap();

        let (p, migrated) = load_prefs_inner(&path);
        assert!(migrated, "a v2 file must be rewritten in the v3 shape");
        assert_eq!(p.style.font.family, "Inter");
        assert_eq!(p.style.font.size_pct, 4.2);
        assert_eq!(p.style.text.highlight, Color::new(1, 2, 3, 255));
        assert_eq!(p.style.text.dim, Color::new(4, 5, 6, 255));
        assert_eq!(p.style.box_style.color, Color::new(7, 8, 9, 255));
        assert_eq!(p.style.box_style.opacity, 0.4);
        assert!(!p.style.box_style.rounded);
        assert_eq!(p.style.box_style.radius_frac, 0.3);
        assert!(!p.style.box_style.shadow);
        assert_eq!(p.style.layout.position_pct, 44.0);
        assert_eq!(p.grouping.words_per_popup, 5);
        assert_eq!(p.active_profile, None);
        assert_eq!(p.model, Model::SmallEn);
        assert!(!p.auto_check_updates);
        assert_eq!(p.last_update_check, Some(99));
    }

    #[test]
    fn a_flat_style_without_a_version_still_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"style":{"font_family":"Nunito","words_per_popup":2}}"#,
        )
        .unwrap();
        let p = load_prefs_from(&path);
        assert_eq!(p.style.font.family, "Nunito");
        assert_eq!(p.grouping.words_per_popup, 2);
    }

    #[test]
    fn out_of_range_values_are_sanitized() {
        let cfg: Config = serde_json::from_str(
            r#"{"style":{"font":{"size_pct":1000.0,"family":"Comic Sans"},"box":{"radius_frac":9.0}},"grouping":{"words_per_popup":99}}"#,
        )
        .unwrap();
        let mut style = cfg.style;
        let mut grouping = cfg.grouping;
        style.sanitize();
        grouping.sanitize();
        assert_eq!(grouping.words_per_popup, 8);
        assert_eq!(style.font.size_pct, 20.0);
        assert_eq!(style.font.family, FONT_FAMILIES[0]);
        assert_eq!(style.box_style.radius_frac, 0.5);
    }

    #[test]
    fn grouping_accepts_a_single_word() {
        let mut g = Grouping { words_per_popup: 0 };
        g.sanitize();
        assert_eq!(g.words_per_popup, 1);
    }

    #[test]
    fn layout_defaults_preserve_single_line_shrink() {
        let l = LayoutStyle::default();
        assert_eq!(l.fit, Fit::Shrink);
        assert_eq!(l.max_lines, 1);
        assert_eq!(l.max_words_per_line, 0);
        assert_eq!(l.line_height, 1.2);
        assert_eq!(l.reserve_lines, 0);
        assert_eq!(l.max_width_pct, 92.0);
        assert_eq!(l.box_width_pct, None);
        assert_eq!(l.box_height_pct, None);
        assert_eq!(l.anchor, Anchor::Center);
        assert_eq!(l.align, Align::Center);
    }

    #[test]
    fn layout_values_are_clamped() {
        let mut s = Style::default();
        s.layout.max_lines = 0;
        s.layout.max_words_per_line = 99;
        s.layout.line_height = 9.0;
        s.layout.reserve_lines = 99;
        s.layout.max_width_pct = 1.0;
        s.layout.box_width_pct = Some(1.0);
        s.layout.box_height_pct = Some(99.0);
        s.sanitize();
        assert_eq!(s.layout.max_lines, 1);
        assert_eq!(s.layout.max_words_per_line, 8);
        assert_eq!(s.layout.line_height, 2.0);
        assert_eq!(s.layout.reserve_lines, 4);
        assert_eq!(s.layout.max_width_pct, 40.0);
        assert_eq!(s.layout.box_width_pct, Some(10.0));
        assert_eq!(s.layout.box_height_pct, Some(60.0));
    }

    #[test]
    fn unknown_layout_enum_values_degrade_to_defaults() {
        let s: Style = serde_json::from_str(
            r#"{"layout":{"anchor":"sideways","align":"diagonal","fit":"stretch"}}"#,
        )
        .unwrap();
        assert_eq!(s.layout.anchor, Anchor::Center);
        assert_eq!(s.layout.align, Align::Center);
        assert_eq!(s.layout.fit, Fit::Shrink);
    }

    #[test]
    fn motion_defaults_to_static() {
        let m = MotionStyle::default();
        assert_eq!(m.popup, PopupEffect::None);
        assert_eq!(m.exit, PopupEffect::None);
        assert_eq!(m.duration_ms, 260);
        assert_eq!(m.exit_duration_ms, 160);
        assert_eq!(m.easing, Easing::EaseOut);
        assert_eq!(m.intensity, 0.20);
    }

    #[test]
    fn motion_round_trips_and_clamps() {
        let mut style = Style::default();
        style.motion.popup = PopupEffect::SlideUp;
        style.motion.exit = PopupEffect::SlideDown;
        style.motion.easing = Easing::Spring;
        style.motion.duration_ms = 9_000;
        style.motion.exit_duration_ms = 9_000;
        style.motion.intensity = 9.0;
        style.sanitize();
        assert_eq!(style.motion.duration_ms, 1200);
        assert_eq!(style.motion.exit_duration_ms, 1200);
        assert_eq!(style.motion.intensity, 1.5);

        let json = serde_json::to_string(&style).unwrap();
        let back: Style = serde_json::from_str(&json).unwrap();
        assert_eq!(back.motion, style.motion);
    }

    #[test]
    fn unknown_motion_values_degrade_to_defaults() {
        let s: Style = serde_json::from_str(
            r#"{"motion":{"popup":"warp","easing":"bouncy","duration_ms":250}}"#,
        )
        .unwrap();
        assert_eq!(s.motion.popup, PopupEffect::None);
        assert_eq!(s.motion.exit, PopupEffect::None);
        assert_eq!(s.motion.easing, Easing::EaseOut);
        assert_eq!(s.motion.duration_ms, 250);
    }

    #[test]
    fn phase_two_effects_default_to_the_old_look() {
        let s = Style::default();
        assert!(s.font.active_bold);
        assert!(!s.font.dim_bold);
        assert!(!s.font.uppercase);
        assert_eq!(s.font.letter_spacing, 0.0);
        assert_eq!(s.text.opacity, 1.0);
        assert!(!s.text.outline.enabled);
        assert!(!s.text.shadow.enabled);
        assert!(!s.text.glow.enabled);
        assert_eq!(s.box_style.padding_x, 0.50);
        assert_eq!(s.box_style.padding_y, 0.24);
        assert_eq!(s.box_style.border_width, 0.0);
        assert_eq!(s.box_style.shadow_opacity, 1.0);
        assert_eq!(s.box_style.shadow_offset_y, 0.10);
        assert_eq!(s.box_style.shadow_blur, 0.14);
        assert_eq!(s.highlight.scale, 1.0);
        assert!(!s.highlight.pill.enabled);
        assert!(!s.highlight.underline.enabled);
    }

    #[test]
    fn phase_two_effects_round_trip_and_clamp() {
        let mut style = Style::default();
        style.font.uppercase = true;
        style.font.letter_spacing = 9.0;
        style.text.opacity = 4.0;
        style.text.outline.enabled = true;
        style.text.outline.width = 9.0;
        style.text.shadow.offset_y = -9.0;
        style.text.shadow.blur = 9.0;
        style.text.glow.radius = 9.0;
        style.box_style.padding_x = 9.0;
        style.box_style.border_width = 9.0;
        style.box_style.shadow_opacity = 9.0;
        style.box_style.shadow_blur = 9.0;
        style.highlight.scale = 9.0;
        style.highlight.pill.padding_x = 9.0;
        style.highlight.underline.width = 9.0;
        style.sanitize();

        assert_eq!(style.font.letter_spacing, 0.3);
        assert_eq!(style.text.opacity, 1.0);
        assert_eq!(style.text.outline.width, 0.3);
        assert_eq!(style.text.shadow.offset_y, -0.5);
        assert_eq!(style.text.shadow.blur, 0.5);
        assert_eq!(style.text.glow.radius, 0.5);
        assert_eq!(style.box_style.padding_x, 2.0);
        assert_eq!(style.box_style.border_width, 0.3);
        assert_eq!(style.box_style.shadow_opacity, 1.0);
        assert_eq!(style.box_style.shadow_blur, 0.5);
        assert_eq!(style.highlight.scale, 1.4);
        assert_eq!(style.highlight.pill.padding_x, 1.0);
        assert_eq!(style.highlight.underline.width, 0.3);

        let json = serde_json::to_string(&style).unwrap();
        let back: Style = serde_json::from_str(&json).unwrap();
        assert_eq!(back, style);
    }
}
