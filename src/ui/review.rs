//! Review screen: transcript editing, style controls, live preview.

use std::path::PathBuf;

use egui::{Color32, RichText, Sense};

use crate::chunk::{self, Chunk};
use crate::config::{
    Align, Anchor, Color, Easing, Fit, Grouping, PopupEffect, Style, FONT_FAMILIES,
};
use crate::pipeline::Ingest;
use crate::probe::{Fftools, VideoInfo};
use crate::render::MotionPhase;
use crate::transcribe::Word;

use super::preview::Preview;
use super::theme;

/// What a panel wants the app to do after handling its own state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Selection or highlighted word changed: re-extract the preview frame.
    Seek,
    /// User pressed Export.
    Export,
    /// words-per-popup changed and chunks were rebuilt.
    Rechunked {
        discarded_edits: bool,
    },
}

/// One caption popup as the user edits it.
///
/// Text is what the user sees and edits; `words` keeps the original whisper
/// timings so untouched chunks export exactly as transcribed (DESIGN: edits
/// re-time evenly within the span, untouched chunks keep whisper timings).
pub struct EditChunk {
    pub text: String,
    pub start_secs: f64,
    pub end_secs: f64,
    pub words: Vec<Word>,
    pub edited: bool,
}

impl EditChunk {
    fn from_chunk(c: &Chunk) -> Self {
        EditChunk {
            text: c.text(),
            start_secs: c.start_secs,
            end_secs: c.end_secs,
            words: c.words.clone(),
            edited: false,
        }
    }

    /// Materialize for export/preview timing.
    fn to_chunk(&self) -> Chunk {
        if self.edited {
            Chunk::from_edited_text(&self.text, self.start_secs, self.end_secs)
        } else {
            Chunk {
                words: self.words.clone(),
                start_secs: self.start_secs,
                end_secs: self.end_secs,
            }
        }
    }
}

/// Everything the review screen operates on.
pub struct ReviewState {
    pub input: PathBuf,
    pub tools: Fftools,
    pub info: VideoInfo,
    pub chunks: Vec<EditChunk>,
    /// Original whisper words — the source for re-chunking.
    pub original_words: Vec<Word>,
    pub selected: usize,
    pub active_word: usize,
    pub style: Style,
    pub grouping: Grouping,
    /// Whether the full style editor sheet is open.
    pub customize_open: bool,
    /// Selected tab in the customize sheet.
    pub customize_tab: usize,
    /// Loop the popup transition in the preview.
    pub motion_playing: bool,
    pub edited_any: bool,
    pub preview: Preview,
    /// Which (chunk, word) the extracted frame belongs to.
    pub frame_key: (usize, usize),
}

impl ReviewState {
    pub fn new(input: PathBuf, ing: Ingest, mut style: Style, mut grouping: Grouping) -> Self {
        style.sanitize();
        grouping.sanitize();
        let chunks = chunk::chunk_words(&ing.words, grouping.words_per_popup)
            .iter()
            .map(EditChunk::from_chunk)
            .collect();
        ReviewState {
            input,
            tools: ing.tools,
            info: ing.info,
            chunks,
            original_words: ing.words,
            selected: 0,
            active_word: 0,
            style,
            grouping,
            customize_open: false,
            customize_tab: 0,
            motion_playing: false,
            edited_any: false,
            preview: Preview::default(),
            frame_key: (usize::MAX, usize::MAX),
        }
    }

    /// Chunks as the exporter sees them (empty captions dropped).
    pub fn to_chunks(&self) -> Vec<Chunk> {
        self.chunks
            .iter()
            .filter(|c| !c.text.trim().is_empty())
            .map(EditChunk::to_chunk)
            .collect()
    }

    /// Preview time: midpoint of the highlighted word's window, i.e. the exact
    /// instant the exporter would show this state.
    pub fn preview_time(&self) -> f64 {
        let Some(c) = self.chunks.get(self.selected) else {
            return 0.0;
        };
        let windows = c.to_chunk().state_windows();
        if windows.is_empty() {
            return (c.start_secs + c.end_secs) / 2.0;
        }
        let (s, e) = windows[self.active_word.min(windows.len() - 1)];
        (s + e) / 2.0
    }

    /// Caption words for the preview (same tokens the export renders).
    pub fn preview_words(&self) -> Vec<String> {
        self.chunks
            .get(self.selected)
            .map(|c| c.text.split_whitespace().map(String::from).collect())
            .unwrap_or_default()
    }

    /// Rebuild chunks from the original whisper words (words-per-popup
    /// changed). Returns whether any user edits were discarded.
    pub fn rechunk(&mut self, words_per: usize) -> bool {
        let discarded = self.edited_any;
        self.chunks = chunk::chunk_words(&self.original_words, words_per)
            .iter()
            .map(EditChunk::from_chunk)
            .collect();
        self.edited_any = false;
        self.selected = self.selected.min(self.chunks.len().saturating_sub(1));
        self.active_word = 0;
        discarded
    }

    fn select_chunk(&mut self, idx: usize) {
        if idx != self.selected {
            self.selected = idx;
            self.active_word = 0;
        }
    }

    fn mark_edited(&mut self, idx: usize) {
        self.edited_any = true;
        let max = self.chunks[idx].text.split_whitespace().count();
        self.active_word = self.active_word.min(max.saturating_sub(1));
        self.preview.invalidate();
    }
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

fn col(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Left column: the composited preview + chunk navigation.
pub fn preview_panel(ui: &mut egui::Ui, r: &mut ReviewState) -> Action {
    let mut action = Action::None;
    let words = r.preview_words();
    let active = r.active_word;
    let has_in = r.style.motion.popup != PopupEffect::None && r.style.motion.duration_ms > 0;
    let has_out = r.style.motion.exit != PopupEffect::None && r.style.motion.exit_duration_ms > 0;
    let animating = r.motion_playing && (has_in || has_out);
    let motion = if animating {
        let in_dur = r.style.motion.duration_ms as f64 / 1000.0;
        let out_dur = r.style.motion.exit_duration_ms as f64 / 1000.0;
        let hold = 0.7;
        let gap = 0.35;
        let cycle = in_dur + hold + out_dur + gap;
        let t = ui.input(|i| i.time) % cycle;
        if has_in && t < in_dur {
            MotionPhase::Entrance((t / in_dur) as f32)
        } else if t < in_dur + hold {
            MotionPhase::Settled
        } else if has_out && t < in_dur + hold + out_dur {
            MotionPhase::Exit(((t - in_dur - hold) / out_dur) as f32)
        } else {
            MotionPhase::Hidden
        }
    } else {
        MotionPhase::Settled
    };
    r.preview
        .ensure(&ui.ctx().clone(), &r.style, &words, active, motion);
    if animating {
        ui.ctx().request_repaint();
    }

    ui.horizontal(|ui| {
        let n = r.chunks.len();
        ui.label(RichText::new(format!("Popup {} of {}", r.selected + 1, n.max(1))).strong());
        if let Some(c) = r.chunks.get(r.selected) {
            ui.label(RichText::new(format!("· {:.1}–{:.1}s", c.start_secs, c.end_secs)).weak());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let has_next = r.selected + 1 < n;
            if theme::button_enabled(ui, has_next, "Next").clicked() && has_next {
                r.select_chunk(r.selected + 1);
                action = Action::Seek;
            }
            let has_prev = r.selected > 0;
            if theme::button_enabled(ui, has_prev, "Previous").clicked() && has_prev {
                r.select_chunk(r.selected - 1);
                action = Action::Seek;
            }
        });
    });
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // Image, fit into the available space preserving aspect.
    let size = r.preview.image_size();
    match size {
        Some((iw, ih)) if r.preview.texture().is_some() => {
            let avail = ui.available_size();
            let scale = (avail.x / iw as f32)
                .min((avail.y - 4.0).max(40.0) / ih as f32)
                .min(1.0);
            let want = egui::vec2(iw as f32 * scale, ih as f32 * scale);
            let (rect, _) = ui.allocate_exact_size(want, Sense::hover());
            if let Some(tex) = r.preview.texture() {
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.painter().rect_stroke(
                    rect,
                    2.0,
                    egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                    egui::StrokeKind::Inside,
                );
            }
        }
        _ => {
            ui.centered_and_justified(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("extracting preview frame…");
            });
        }
    }

    if let Some(err) = &r.preview.caption_error {
        ui.add_space(4.0);
        ui.colored_label(ui.visuals().error_fg_color, err);
    }

    // Highlighted word, clickable.
    if !words.is_empty() {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("highlight:").weak());
            for (i, w) in words.iter().enumerate() {
                let on = i == r.active_word;
                let base = if on {
                    r.style.text.highlight
                } else {
                    r.style.text.dim
                };
                let mut text = RichText::new(w.as_str()).color(col(base));
                if on {
                    text = text.strong();
                }
                if ui
                    .add(egui::Label::new(text).sense(Sense::click()))
                    .clicked()
                {
                    r.active_word = i;
                    action = Action::Seek;
                }
            }
        });
    }
    action
}

/// Right column: profile picker plus style controls. Returns the action the
/// app must run.
pub fn style_panel(
    ui: &mut egui::Ui,
    r: &mut ReviewState,
    profiles: &crate::profile::ProfileStore,
    profile_ui: &mut super::profile::ProfileUi,
) -> Action {
    let mut action = Action::None;
    let before = r.style.clone();

    // The profile strip stays visible while the essentials scroll.
    egui::Panel::top("style_profile").show(ui, |ui| {
        action = join(action, profile_ui.section(ui, r, profiles));
    });

    // Export is pinned to the bottom of the panel (it sizes to its content);
    // the essentials sit above it.
    egui::Panel::bottom("style_export").show(ui, |ui| {
        ui.add_space(6.0);
        let exportable = r.chunks.iter().any(|c| !c.text.trim().is_empty());
        if theme::primary_button(ui, exportable, "Export video").clicked() {
            action = Action::Export;
        }
        ui.add_space(4.0);
        let hint = if !exportable {
            "nothing to export"
        } else if cfg!(target_os = "macos") {
            "or press \u{2318}E"
        } else {
            "or press Ctrl+E"
        };
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(hint).weak().small());
        });
    });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(8.0);
            essentials(ui, r);
            ui.add_space(14.0);
            if theme::wide_button(ui, true, "Customize…").clicked() {
                r.customize_open = true;
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Outlines, box detail, wrapping and motion timing live in Customize.",
                )
                .small()
                .weak(),
            );
            ui.add_space(8.0);
        });

    action = join(action, customize_window(ui.ctx(), r));
    action = join(action, profile_ui.windows(ui.ctx(), r, profiles));

    r.style.sanitize();
    r.grouping.sanitize();
    if r.style != before {
        r.preview.invalidate();
    }
    action
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BackgroundChoice {
    None,
    Box,
    Pill,
}

impl BackgroundChoice {
    fn label(self) -> &'static str {
        match self {
            BackgroundChoice::None => "None",
            BackgroundChoice::Box => "Box",
            BackgroundChoice::Pill => "Pill",
        }
    }
}

fn background_choice(s: &Style) -> BackgroundChoice {
    if s.highlight.pill.enabled {
        BackgroundChoice::Pill
    } else if s.box_style.opacity > 0.01 && s.box_style.color.a > 0 {
        BackgroundChoice::Box
    } else {
        BackgroundChoice::None
    }
}

fn set_background(s: &mut Style, choice: BackgroundChoice) {
    match choice {
        BackgroundChoice::None => {
            s.box_style.opacity = 0.0;
            s.box_style.shadow = false;
            s.highlight.pill.enabled = false;
        }
        BackgroundChoice::Box => {
            s.highlight.pill.enabled = false;
            if s.box_style.opacity <= 0.01 {
                s.box_style.opacity = 0.72;
            }
            if s.box_style.color.a == 0 {
                s.box_style.color.a = 255;
            }
            s.box_style.shadow = true;
        }
        BackgroundChoice::Pill => {
            s.box_style.opacity = 0.0;
            s.box_style.shadow = false;
            s.highlight.pill.enabled = true;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MotionPreset {
    None,
    Pop,
    SlideUp,
    Fade,
    Custom,
}

impl MotionPreset {
    fn label(self) -> &'static str {
        match self {
            MotionPreset::None => "None",
            MotionPreset::Pop => "Pop",
            MotionPreset::SlideUp => "Slide up",
            MotionPreset::Fade => "Fade",
            MotionPreset::Custom => "Custom",
        }
    }
}

fn motion_preset(s: &Style) -> MotionPreset {
    let m = &s.motion;
    if m.popup == PopupEffect::None && m.exit == PopupEffect::None {
        MotionPreset::None
    } else if m.popup == PopupEffect::Pop && m.exit == PopupEffect::Pop {
        MotionPreset::Pop
    } else if m.popup == PopupEffect::SlideUp && m.exit == PopupEffect::SlideDown {
        MotionPreset::SlideUp
    } else if m.popup == PopupEffect::Fade && m.exit == PopupEffect::Fade {
        MotionPreset::Fade
    } else {
        MotionPreset::Custom
    }
}

fn apply_motion_preset(s: &mut Style, preset: MotionPreset) {
    let m = &mut s.motion;
    match preset {
        MotionPreset::None => {
            m.popup = PopupEffect::None;
            m.exit = PopupEffect::None;
        }
        MotionPreset::Pop => {
            m.popup = PopupEffect::Pop;
            m.exit = PopupEffect::Pop;
            m.duration_ms = 260;
            m.exit_duration_ms = 180;
            m.easing = Easing::EaseOut;
            m.intensity = 0.20;
        }
        MotionPreset::SlideUp => {
            m.popup = PopupEffect::SlideUp;
            m.exit = PopupEffect::SlideDown;
            m.duration_ms = 280;
            m.exit_duration_ms = 200;
            m.easing = Easing::EaseOut;
            m.intensity = 0.60;
        }
        MotionPreset::Fade => {
            m.popup = PopupEffect::Fade;
            m.exit = PopupEffect::Fade;
            m.duration_ms = 220;
            m.exit_duration_ms = 160;
            m.easing = Easing::EaseOut;
            m.intensity = 0.0;
        }
        MotionPreset::Custom => {}
    }
}

fn essentials(ui: &mut egui::Ui, r: &mut ReviewState) {
    let s = &mut r.style;
    theme::section(ui, "Essentials");
    ui.add_space(4.0);
    egui::Grid::new("essentials_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Font");
            egui::ComboBox::from_id_salt("essentials_font")
                .selected_text(s.font.family.clone())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for fam in FONT_FAMILIES {
                        ui.selectable_value(&mut s.font.family, fam.to_string(), fam);
                    }
                });
            ui.end_row();

            ui.label("Size");
            ui.add(
                egui::Slider::new(&mut s.font.size_pct, 1.0..=20.0)
                    .fixed_decimals(1)
                    .suffix("%"),
            );
            ui.end_row();

            ui.label("Highlight");
            color_swatch(ui, &mut s.text.highlight);
            ui.end_row();

            ui.label("Background");
            let mut bg = background_choice(s);
            egui::ComboBox::from_id_salt("essentials_background")
                .selected_text(bg.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for choice in [
                        BackgroundChoice::None,
                        BackgroundChoice::Box,
                        BackgroundChoice::Pill,
                    ] {
                        if ui.selectable_label(bg == choice, choice.label()).clicked() {
                            bg = choice;
                            set_background(s, choice);
                        }
                    }
                });
            ui.end_row();

            ui.label("Position");
            ui.add(
                egui::Slider::new(&mut s.layout.position_pct, 5.0..=95.0)
                    .fixed_decimals(0)
                    .suffix("%"),
            );
            ui.end_row();

            ui.label("Motion");
            let current = motion_preset(s);
            egui::ComboBox::from_id_salt("essentials_motion")
                .selected_text(current.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for preset in [
                        MotionPreset::None,
                        MotionPreset::Pop,
                        MotionPreset::SlideUp,
                        MotionPreset::Fade,
                    ] {
                        if ui
                            .selectable_label(current == preset, preset.label())
                            .clicked()
                        {
                            apply_motion_preset(s, preset);
                        }
                    }
                });
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new("Start from a profile, then tune these. Everything else is under Customize.")
            .small()
            .weak(),
    );
}

/// The full editor sheet. It is a window rather than a panel so the review
/// screen keeps its short essentials list.
fn customize_window(ctx: &egui::Context, r: &mut ReviewState) -> Action {
    if !r.customize_open {
        return Action::None;
    }
    let mut open = r.customize_open;
    let mut action = Action::None;
    egui::Window::new("Customize style")
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 16.0))
        .default_width(720.0)
        .default_height(680.0)
        .min_width(600.0)
        .resizable(true)
        .show(ctx, |ui| {
            let mut tab = r.customize_tab.min(2);
            if theme::segmented(ui, &mut tab, &["Look", "Layout", "Motion"]) {
                r.customize_tab = tab;
            }
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match r.customize_tab {
                        0 => {
                            let style = &mut r.style;
                            ui.columns(2, |cols| {
                                theme::section(&mut cols[0], "Text");
                                text_basics(&mut cols[0], style);
                                cols[0].add_space(16.0);
                                theme::section(&mut cols[0], "Active word");
                                highlight_tab(&mut cols[0], style);

                                theme::section(&mut cols[1], "Box");
                                box_tab(&mut cols[1], style);
                                cols[1].add_space(16.0);
                                theme::section(&mut cols[1], "Effects");
                                effects_tab(&mut cols[1], style);
                            });
                        }
                        1 => action = join(action, layout_tab(ui, r)),
                        _ => motion_tab(ui, r),
                    }
                    ui.add_space(16.0);
                    if theme::wide_button(
                        ui,
                        r.style != Style::default(),
                        "Reset style to defaults",
                    )
                    .clicked()
                    {
                        r.style = Style::default();
                        r.preview.invalidate();
                        action = Action::Seek;
                    }
                    ui.add_space(8.0);
                });
        });
    r.customize_open = open;
    action
}

/// Combine two panel actions without losing the more significant one.
fn join(a: Action, b: Action) -> Action {
    use Action::*;
    match (a, b) {
        (Export, _) | (_, Export) => Export,
        (Rechunked { discarded_edits }, _) => Rechunked { discarded_edits },
        (_, Rechunked { discarded_edits }) => Rechunked { discarded_edits },
        (Seek, _) | (_, Seek) => Seek,
        _ => None,
    }
}

fn text_basics(ui: &mut egui::Ui, s: &mut Style) {
    egui::Grid::new("text_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Font");
            egui::ComboBox::from_id_salt("font_family")
                .selected_text(s.font.family.clone())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for fam in FONT_FAMILIES {
                        ui.selectable_value(&mut s.font.family, fam.to_string(), fam);
                    }
                });
            ui.end_row();

            ui.label("Size");
            ui.add(
                egui::Slider::new(&mut s.font.size_pct, 1.0..=20.0)
                    .fixed_decimals(1)
                    .suffix("%"),
            );
            ui.end_row();

            ui.label("Dim");
            color_swatch(ui, &mut s.text.dim);
            ui.end_row();

            ui.label("Bold rest");
            ui.checkbox(&mut s.font.dim_bold, "");
            ui.end_row();

            ui.label("Caps");
            ui.checkbox(&mut s.font.uppercase, "");
            ui.end_row();

            ui.label("Tracking");
            ui.add(
                egui::Slider::new(&mut s.font.letter_spacing, 0.0..=0.20)
                    .fixed_decimals(2)
                    .suffix("em"),
            );
            ui.end_row();

            ui.label("Opacity");
            ui.add(egui::Slider::new(&mut s.text.opacity, 0.0..=1.0).fixed_decimals(2));
            ui.end_row();
        });
}

fn effects_tab(ui: &mut egui::Ui, s: &mut Style) {
    theme::section(ui, "Outline");
    ui.add_space(4.0);
    ui.checkbox(&mut s.text.outline.enabled, "Enable outline");
    if s.text.outline.enabled {
        ui.add_space(4.0);
        egui::Grid::new("outline_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("Color");
                color_swatch(ui, &mut s.text.outline.color);
                ui.end_row();
                ui.label("Width");
                ui.add(
                    egui::Slider::new(&mut s.text.outline.width, 0.01..=0.25)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
            });
    }

    ui.add_space(12.0);
    theme::section(ui, "Text shadow");
    ui.add_space(4.0);
    ui.checkbox(&mut s.text.shadow.enabled, "Enable text shadow");
    if s.text.shadow.enabled {
        ui.add_space(4.0);
        egui::Grid::new("shadow_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("Color");
                color_swatch(ui, &mut s.text.shadow.color);
                ui.end_row();
                ui.label("Offset X");
                ui.add(
                    egui::Slider::new(&mut s.text.shadow.offset_x, -0.30..=0.30)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
                ui.label("Offset Y");
                ui.add(
                    egui::Slider::new(&mut s.text.shadow.offset_y, -0.30..=0.30)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
                ui.label("Blur");
                ui.add(
                    egui::Slider::new(&mut s.text.shadow.blur, 0.0..=0.30)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
            });
    }

    ui.add_space(12.0);
    theme::section(ui, "Glow");
    ui.add_space(4.0);
    ui.checkbox(&mut s.text.glow.enabled, "Enable glow");
    if s.text.glow.enabled {
        ui.add_space(4.0);
        egui::Grid::new("glow_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("Color");
                color_swatch(ui, &mut s.text.glow.color);
                ui.end_row();
                ui.label("Radius");
                ui.add(
                    egui::Slider::new(&mut s.text.glow.radius, 0.02..=0.40)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
            });
    }
}

fn box_tab(ui: &mut egui::Ui, s: &mut Style) {
    egui::Grid::new("box_fill_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Fill");
            color_swatch(ui, &mut s.box_style.color);
            ui.end_row();

            ui.label("Opacity");
            ui.add(egui::Slider::new(&mut s.box_style.opacity, 0.0..=1.0).fixed_decimals(2));
            ui.end_row();

            ui.label("Rounded");
            ui.checkbox(&mut s.box_style.rounded, "");
            ui.end_row();

            if s.box_style.rounded {
                ui.label("Radius");
                ui.add(
                    egui::Slider::new(&mut s.box_style.radius_frac, 0.0..=0.5).fixed_decimals(2),
                );
                ui.end_row();
            }

            ui.label("Padding X");
            ui.add(
                egui::Slider::new(&mut s.box_style.padding_x, 0.0..=1.50)
                    .fixed_decimals(2)
                    .suffix("em"),
            );
            ui.end_row();

            ui.label("Padding Y");
            ui.add(
                egui::Slider::new(&mut s.box_style.padding_y, 0.0..=1.00)
                    .fixed_decimals(2)
                    .suffix("em"),
            );
            ui.end_row();
        });

    ui.add_space(14.0);
    theme::section(ui, "Border");
    ui.add_space(4.0);
    egui::Grid::new("border_grid")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Width");
            ui.add(
                egui::Slider::new(&mut s.box_style.border_width, 0.0..=0.25)
                    .fixed_decimals(2)
                    .suffix("em"),
            );
            ui.end_row();
            ui.label("Color");
            color_swatch(ui, &mut s.box_style.border_color);
            ui.end_row();
        });

    ui.add_space(12.0);
    theme::section(ui, "Box shadow");
    ui.add_space(4.0);
    ui.checkbox(&mut s.box_style.shadow, "Enable box shadow");
    if s.box_style.shadow {
        ui.add_space(4.0);
        egui::Grid::new("box_shadow_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("Color");
                color_swatch(ui, &mut s.box_style.shadow_color);
                ui.end_row();
                ui.label("Opacity");
                ui.add(
                    egui::Slider::new(&mut s.box_style.shadow_opacity, 0.0..=1.0).fixed_decimals(2),
                );
                ui.end_row();
                ui.label("Offset Y");
                ui.add(
                    egui::Slider::new(&mut s.box_style.shadow_offset_y, -0.30..=0.30)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
                ui.label("Blur");
                ui.add(
                    egui::Slider::new(&mut s.box_style.shadow_blur, 0.0..=0.30)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
            });
    }
}

fn highlight_tab(ui: &mut egui::Ui, s: &mut Style) {
    egui::Grid::new("highlight_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Color");
            color_swatch(ui, &mut s.text.highlight);
            ui.end_row();

            ui.label("Bold active");
            ui.checkbox(&mut s.font.active_bold, "");
            ui.end_row();

            ui.label("Scale");
            ui.add(
                egui::Slider::new(&mut s.highlight.scale, 1.0..=1.4)
                    .fixed_decimals(2)
                    .suffix("×"),
            );
            ui.end_row();
        });

    ui.add_space(14.0);
    theme::section(ui, "Pill");
    ui.add_space(4.0);
    ui.checkbox(
        &mut s.highlight.pill.enabled,
        "Plate behind the active word",
    );
    if s.highlight.pill.enabled {
        ui.add_space(4.0);
        egui::Grid::new("pill_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("Color");
                color_swatch(ui, &mut s.highlight.pill.color);
                ui.end_row();
                ui.label("Padding X");
                ui.add(
                    egui::Slider::new(&mut s.highlight.pill.padding_x, 0.0..=0.60)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
                ui.label("Padding Y");
                ui.add(
                    egui::Slider::new(&mut s.highlight.pill.padding_y, 0.0..=0.60)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
                ui.label("Radius");
                ui.add(
                    egui::Slider::new(&mut s.highlight.pill.radius_frac, 0.0..=1.0)
                        .fixed_decimals(2),
                );
                ui.end_row();
            });
    }

    ui.add_space(12.0);
    theme::section(ui, "Underline");
    ui.add_space(4.0);
    ui.checkbox(
        &mut s.highlight.underline.enabled,
        "Underline the active word",
    );
    if s.highlight.underline.enabled {
        ui.add_space(4.0);
        egui::Grid::new("underline_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("Color");
                color_swatch(ui, &mut s.highlight.underline.color);
                ui.end_row();
                ui.label("Width");
                ui.add(
                    egui::Slider::new(&mut s.highlight.underline.width, 0.01..=0.20)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
                ui.label("Offset");
                ui.add(
                    egui::Slider::new(&mut s.highlight.underline.offset, 0.0..=0.20)
                        .fixed_decimals(2)
                        .suffix("em"),
                );
                ui.end_row();
            });
    }
}

fn fit_label(fit: Fit) -> &'static str {
    match fit {
        Fit::Shrink => "Shrink to one line",
        Fit::Wrap => "Fixed size, wrap",
        Fit::Clip => "Fixed size, clip",
    }
}

fn align_label(align: Align) -> &'static str {
    match align {
        Align::Center => "Center",
        Align::Left => "Left",
        Align::Right => "Right",
    }
}

fn anchor_label(anchor: Anchor) -> &'static str {
    match anchor {
        Anchor::Center => "Box center",
        Anchor::Top => "Box top",
        Anchor::Bottom => "Box bottom",
    }
}

fn layout_tab(ui: &mut egui::Ui, r: &mut ReviewState) -> Action {
    let mut action = Action::None;
    let s = &mut r.style;

    egui::Grid::new("layout_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Fit");
            let was_shrink = s.layout.fit == Fit::Shrink;
            egui::ComboBox::from_id_salt("layout_fit")
                .selected_text(fit_label(s.layout.fit))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for fit in [Fit::Shrink, Fit::Wrap, Fit::Clip] {
                        ui.selectable_value(&mut s.layout.fit, fit, fit_label(fit));
                    }
                });
            if was_shrink && s.layout.fit == Fit::Wrap && s.layout.max_lines < 2 {
                s.layout.max_lines = 2;
            }
            ui.end_row();

            ui.label("Max lines");
            ui.add_enabled(
                s.layout.fit != Fit::Shrink,
                egui::Slider::new(&mut s.layout.max_lines, 1..=4)
                    .step_by(1.0)
                    .fixed_decimals(0),
            );
            ui.end_row();

            ui.label("Words/line");
            ui.add_enabled(
                s.layout.fit != Fit::Shrink,
                egui::Slider::new(&mut s.layout.max_words_per_line, 0..=8)
                    .step_by(1.0)
                    .fixed_decimals(0),
            );
            ui.end_row();

            ui.label("Line height");
            ui.add(
                egui::Slider::new(&mut s.layout.line_height, 1.0..=2.0)
                    .step_by(0.05)
                    .fixed_decimals(2)
                    .suffix("×"),
            );
            ui.end_row();

            ui.label("Reserve lines");
            ui.add(
                egui::Slider::new(&mut s.layout.reserve_lines, 0..=4)
                    .step_by(1.0)
                    .fixed_decimals(0),
            );
            ui.end_row();

            ui.label("Align");
            egui::ComboBox::from_id_salt("layout_align")
                .selected_text(align_label(s.layout.align))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for align in [Align::Center, Align::Left, Align::Right] {
                        ui.selectable_value(&mut s.layout.align, align, align_label(align));
                    }
                });
            ui.end_row();

            ui.label("Anchor");
            egui::ComboBox::from_id_salt("layout_anchor")
                .selected_text(anchor_label(s.layout.anchor))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for anchor in [Anchor::Center, Anchor::Top, Anchor::Bottom] {
                        ui.selectable_value(&mut s.layout.anchor, anchor, anchor_label(anchor));
                    }
                });
            ui.end_row();

            ui.label("Position");
            ui.add(
                egui::Slider::new(&mut s.layout.position_pct, 5.0..=95.0)
                    .fixed_decimals(0)
                    .suffix("%"),
            );
            ui.end_row();

            ui.label("Max width");
            ui.add_enabled(
                s.layout.box_width_pct.is_none(),
                egui::Slider::new(&mut s.layout.max_width_pct, 40.0..=100.0)
                    .fixed_decimals(0)
                    .suffix("%"),
            );
            ui.end_row();
        });

    ui.add_space(6.0);
    ui.label(
        RichText::new(match s.layout.fit {
            Fit::Shrink => "One line; the font shrinks until the text fits.",
            Fit::Wrap => "Keeps the size and wraps; shrinks only if the words cannot fit.",
            Fit::Clip => "Keeps the size and drops words or lines that do not fit.",
        })
        .small()
        .weak(),
    );
    ui.label(
        RichText::new(
            "Words/line 0 means wrap by width only. Reserve 0 means natural height; set it to Max lines for a fixed-height box.",
        )
        .small()
        .weak(),
    );
    ui.label(
        RichText::new("Position places the selected box anchor at that % of frame height.")
            .small()
            .weak(),
    );

    ui.add_space(14.0);
    theme::section(ui, "Fixed box");
    ui.add_space(4.0);

    let mut fixed_w = s.layout.box_width_pct.is_some();
    if ui.checkbox(&mut fixed_w, "Fixed width").changed() {
        s.layout.box_width_pct = fixed_w.then_some(80.0);
    }
    if let Some(w) = &mut s.layout.box_width_pct {
        ui.add(
            egui::Slider::new(w, 20.0..=100.0)
                .fixed_decimals(0)
                .suffix("%"),
        );
    }

    let mut fixed_h = s.layout.box_height_pct.is_some();
    if ui.checkbox(&mut fixed_h, "Fixed height").changed() {
        s.layout.box_height_pct = fixed_h.then_some(12.0);
    }
    if let Some(h) = &mut s.layout.box_height_pct {
        ui.add(
            egui::Slider::new(h, 5.0..=60.0)
                .fixed_decimals(0)
                .suffix("%"),
        );
    }

    ui.add_space(14.0);
    theme::section(ui, "Grouping");
    ui.add_space(4.0);
    egui::Grid::new("layout_grouping_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(86.0)
        .show(ui, |ui| {
            ui.label("Words");
            // Words-per-popup re-chunks from whisper's words, discarding edits
            // (confirmed M3 decision, with a warning banner).
            let mut wpp = r.grouping.words_per_popup as f32;
            let resp = ui.add(
                egui::Slider::new(&mut wpp, 1.0..=8.0)
                    .step_by(1.0)
                    .fixed_decimals(0),
            );
            let new_wpp = wpp as usize;
            if resp.changed() && new_wpp != r.grouping.words_per_popup {
                r.grouping.words_per_popup = new_wpp;
                let discarded = r.rechunk(new_wpp);
                action = Action::Rechunked {
                    discarded_edits: discarded,
                };
            }
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Changing words per popup re-groups the caption and can discard transcript edits.",
        )
        .small()
        .weak(),
    );
    action
}

fn popup_label(effect: PopupEffect) -> &'static str {
    match effect {
        PopupEffect::None => "None",
        PopupEffect::Pop => "Pop",
        PopupEffect::SlideUp => "Slide up",
        PopupEffect::SlideDown => "Slide down",
        PopupEffect::SlideLeft => "Slide left",
        PopupEffect::SlideRight => "Slide right",
        PopupEffect::Fade => "Fade",
    }
}

fn easing_label(easing: Easing) -> &'static str {
    match easing {
        Easing::Linear => "Linear",
        Easing::EaseOut => "Ease out",
        Easing::EaseInOut => "Ease in-out",
        Easing::Spring => "Spring (overshoot)",
    }
}

fn motion_tab(ui: &mut egui::Ui, r: &mut ReviewState) {
    let any_effect =
        r.style.motion.popup != PopupEffect::None || r.style.motion.exit != PopupEffect::None;
    let can_motion = (r.style.motion.popup != PopupEffect::None && r.style.motion.duration_ms > 0)
        || (r.style.motion.exit != PopupEffect::None && r.style.motion.exit_duration_ms > 0);
    let mut play = r.motion_playing && can_motion;
    {
        let s = &mut r.style;
        theme::section(ui, "Popup motion");
        ui.add_space(4.0);
        egui::Grid::new("motion_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .min_col_width(86.0)
            .show(ui, |ui| {
                ui.label("In");
                let old_in = s.motion.popup;
                egui::ComboBox::from_id_salt("motion_in")
                    .selected_text(popup_label(s.motion.popup))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for effect in [
                            PopupEffect::None,
                            PopupEffect::Pop,
                            PopupEffect::SlideUp,
                            PopupEffect::SlideDown,
                            PopupEffect::SlideLeft,
                            PopupEffect::SlideRight,
                            PopupEffect::Fade,
                        ] {
                            ui.selectable_value(&mut s.motion.popup, effect, popup_label(effect));
                        }
                    });
                // Picking an entrance for the first time gives it a matching
                // exit; the user can still set Out to None afterwards.
                if old_in == PopupEffect::None
                    && s.motion.popup != PopupEffect::None
                    && s.motion.exit == PopupEffect::None
                {
                    s.motion.exit = s.motion.popup;
                }
                ui.end_row();

                ui.label("Out");
                egui::ComboBox::from_id_salt("motion_out")
                    .selected_text(popup_label(s.motion.exit))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for effect in [
                            PopupEffect::None,
                            PopupEffect::Pop,
                            PopupEffect::SlideUp,
                            PopupEffect::SlideDown,
                            PopupEffect::SlideLeft,
                            PopupEffect::SlideRight,
                            PopupEffect::Fade,
                        ] {
                            ui.selectable_value(&mut s.motion.exit, effect, popup_label(effect));
                        }
                    });
                ui.end_row();

                ui.label("In duration");
                ui.add_enabled(
                    s.motion.popup != PopupEffect::None,
                    egui::Slider::new(&mut s.motion.duration_ms, 0..=800)
                        .step_by(10.0)
                        .suffix(" ms"),
                );
                ui.end_row();

                ui.label("Out duration");
                ui.add_enabled(
                    s.motion.exit != PopupEffect::None,
                    egui::Slider::new(&mut s.motion.exit_duration_ms, 0..=800)
                        .step_by(10.0)
                        .suffix(" ms"),
                );
                ui.end_row();

                ui.label("Easing");
                ui.add_enabled_ui(any_effect, |ui| {
                    egui::ComboBox::from_id_salt("motion_easing")
                        .selected_text(easing_label(s.motion.easing))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for easing in [
                                Easing::Linear,
                                Easing::EaseOut,
                                Easing::EaseInOut,
                                Easing::Spring,
                            ] {
                                ui.selectable_value(
                                    &mut s.motion.easing,
                                    easing,
                                    easing_label(easing),
                                );
                            }
                        });
                });
                ui.end_row();

                ui.label("Amount");
                ui.add_enabled(
                    any_effect,
                    egui::Slider::new(&mut s.motion.intensity, 0.0..=1.0).fixed_decimals(2),
                );
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Pop: how much it scales. Slide: distance in font heights. Fade ignores the amount.",
            )
            .small()
            .weak(),
        );
    }

    ui.add_space(10.0);
    ui.add_enabled(can_motion, egui::Checkbox::new(&mut play, "Play preview"));
    r.motion_playing = play && can_motion;
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "The preview plays in, holds, then out. Word-level karaoke effects come later.",
        )
        .small()
        .weak(),
    );
}

/// A colour swatch button (grid cell) bound to a style [`Color`].
fn color_swatch(ui: &mut egui::Ui, value: &mut Color) {
    let mut c = egui::Color32::from_rgba_unmultiplied(value.r, value.g, value.b, value.a);
    if ui.color_edit_button_srgba(&mut c).changed() {
        *value = Color::new(c.r(), c.g(), c.b(), c.a());
    }
}

/// Center column: the editable chunk list. Returns the action the app must run.
pub fn chunk_list(ui: &mut egui::Ui, r: &mut ReviewState) -> Action {
    let mut action = Action::None;

    ui.horizontal(|ui| {
        ui.heading("Transcript");
        ui.add_space(2.0);
        ui.label(
            RichText::new(format!(
                "{} popups · click a row to preview · text is editable",
                r.chunks.len()
            ))
            .weak(),
        );
    });
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for i in 0..r.chunks.len() {
                let is_sel = i == r.selected;
                let fill = if is_sel {
                    ui.visuals().faint_bg_color
                } else {
                    Color32::TRANSPARENT
                };
                let dur = r.chunks[i].end_secs - r.chunks[i].start_secs;
                let edited = r.chunks[i].edited;

                let row = egui::Frame::new()
                    .fill(fill)
                    .corner_radius(theme::ROUND)
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let badge = ui.add_sized(
                                [30.0, theme::CONTROL_H],
                                egui::Label::new(
                                    RichText::new(format!("{}", i + 1)).weak().monospace(),
                                )
                                .sense(Sense::click()),
                            );
                            let label = if edited {
                                RichText::new(format!("{dur:.1}s ✎")).weak().small()
                            } else {
                                RichText::new(format!("{dur:.1}s")).weak().small()
                            };
                            let stamp = ui.add_sized(
                                [46.0, theme::CONTROL_H],
                                egui::Label::new(label).sense(Sense::click()),
                            );
                            let te = ui.add(
                                egui::TextEdit::singleline(&mut r.chunks[i].text)
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::Margin::symmetric(8, 4))
                                    .hint_text("(empty caption)"),
                            );
                            if te.changed() {
                                r.chunks[i].edited = true;
                                r.mark_edited(i);
                            }
                            let click = badge.clicked() || stamp.clicked() || te.clicked();
                            if click && !is_sel {
                                r.select_chunk(i);
                                action = Action::Seek;
                            }
                        });

                        if is_sel {
                            // Word chips: click to preview that highlight.
                            let chips: Vec<String> = r.chunks[i]
                                .text
                                .split_whitespace()
                                .map(String::from)
                                .collect();
                            if !chips.is_empty() {
                                ui.add_space(2.0);
                                ui.horizontal_wrapped(|ui| {
                                    for (wi, w) in chips.iter().enumerate() {
                                        let on = wi == r.active_word;
                                        let base = if on {
                                            r.style.text.highlight
                                        } else {
                                            r.style.text.dim
                                        };
                                        let mut text = RichText::new(w.as_str()).color(col(base));
                                        if on {
                                            text = text.strong();
                                        }
                                        if ui
                                            .add(egui::Label::new(text).sense(Sense::click()))
                                            .clicked()
                                        {
                                            r.active_word = wi;
                                            action = Action::Seek;
                                        }
                                    }
                                });
                            }
                        }
                    });

                // Accent bar down the left edge of the selected row.
                if is_sel {
                    let rect = row.response.rect;
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(rect.left(), rect.top() + 3.0),
                            egui::vec2(3.0, (rect.height() - 6.0).max(1.0)),
                        ),
                        1.5,
                        theme::ACCENT,
                    );
                }
            }
        });

    action
}
