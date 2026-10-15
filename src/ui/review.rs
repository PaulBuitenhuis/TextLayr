//! Review screen: transcript editing, style controls, live preview.

use std::path::PathBuf;

use egui::{Color32, RichText, Sense};

use crate::chunk::{self, Chunk};
use crate::config::{Color, Style, FONT_FAMILIES};
use crate::pipeline::Ingest;
use crate::probe::{Fftools, VideoInfo};
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
    Rechunked { discarded_edits: bool },
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
    pub edited_any: bool,
    pub preview: Preview,
    /// Which (chunk, word) the extracted frame belongs to.
    pub frame_key: (usize, usize),
}

impl ReviewState {
    pub fn new(input: PathBuf, ing: Ingest, mut style: Style) -> Self {
        style.sanitize();
        let chunks = chunk::chunk_words(&ing.words, style.words_per_popup)
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
    r.preview.ensure(&ui.ctx().clone(), &r.style, &words, active);

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
                let base = if on { r.style.highlight } else { r.style.dim };
                let mut text = RichText::new(w.as_str()).color(col(base));
                if on {
                    text = text.strong();
                }
                if ui.add(egui::Label::new(text).sense(Sense::click())).clicked() {
                    r.active_word = i;
                    action = Action::Seek;
                }
            }
        });
    }
    action
}

/// Right column: style controls. Returns the action the app must run.
pub fn style_panel(ui: &mut egui::Ui, r: &mut ReviewState) -> Action {
    let mut action = Action::None;
    let before = r.style.clone();

    // Export is pinned to the bottom of the panel (it sizes to its content);
    // the controls scroll above it.
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
            ui.heading("Style");
            ui.add_space(10.0);

            egui::Grid::new("style_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .min_col_width(86.0)
                .show(ui, |ui| {
                    ui.label("Font");
                    egui::ComboBox::from_id_salt("font_family")
                        .selected_text(r.style.font_family.clone())
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for fam in FONT_FAMILIES {
                                ui.selectable_value(&mut r.style.font_family, fam.to_string(), fam);
                            }
                        });
                    ui.end_row();

                    ui.label("Size");
                    ui.add(
                        egui::Slider::new(&mut r.style.font_size_pct, 1.0..=20.0)
                            .fixed_decimals(1)
                            .suffix("%"),
                    );
                    ui.end_row();

                    ui.label("Position");
                    ui.add(
                        egui::Slider::new(&mut r.style.position_pct, 5.0..=95.0)
                            .fixed_decimals(0)
                            .suffix("%"),
                    );
                    ui.end_row();

                    ui.label("Words");
                    // Words-per-popup re-chunks from whisper's words, discarding
                    // edits (confirmed M3 decision, with a warning banner).
                    let mut wpp = r.style.words_per_popup as f32;
                    let resp = ui.add(
                        egui::Slider::new(&mut wpp, 2.0..=6.0)
                            .step_by(1.0)
                            .fixed_decimals(0),
                    );
                    let new_wpp = wpp as usize;
                    if resp.changed() && new_wpp != r.style.words_per_popup {
                        r.style.words_per_popup = new_wpp;
                        let discarded = r.rechunk(new_wpp);
                        action = Action::Rechunked {
                            discarded_edits: discarded,
                        };
                    }
                    ui.end_row();
                });

            ui.add_space(16.0);
            theme::section(ui, "Colors");
            ui.add_space(8.0);
            egui::Grid::new("colour_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .min_col_width(86.0)
                .show(ui, |ui| {
                    ui.label("Highlight");
                    color_swatch(ui, &mut r.style.highlight);
                    ui.end_row();
                    ui.label("Dim");
                    color_swatch(ui, &mut r.style.dim);
                    ui.end_row();
                    ui.label("Box");
                    color_swatch(ui, &mut r.style.box_color);
                    ui.end_row();
                    ui.label("Opacity");
                    ui.add(egui::Slider::new(&mut r.style.box_opacity, 0.0..=1.0).fixed_decimals(2));
                    ui.end_row();
                });

            ui.add_space(16.0);
            theme::section(ui, "BOX");
            ui.add_space(8.0);
            ui.checkbox(&mut r.style.rounded, "Rounded corners");
            if r.style.rounded {
                ui.add(
                    egui::Slider::new(&mut r.style.corner_radius_frac, 0.0..=0.5)
                        .fixed_decimals(2)
                        .text("Corner radius"),
                );
            }
            ui.checkbox(&mut r.style.shadow, "Drop shadow");

            ui.add_space(16.0);
            if theme::wide_button(ui, r.style != Style::default(), "Reset to defaults").clicked() {
                r.style = Style::default();
                r.preview.invalidate();
                if r.style.words_per_popup != before.words_per_popup {
                    let discarded = r.rechunk(r.style.words_per_popup);
                    action = Action::Rechunked {
                        discarded_edits: discarded,
                    };
                } else {
                    action = Action::Seek;
                }
            }
            ui.add_space(8.0);
        });

    r.style.sanitize();
    if r.style != before {
        r.preview.invalidate();
    }
    action
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
                            let chips: Vec<String> =
                                r.chunks[i].text.split_whitespace().map(String::from).collect();
                            if !chips.is_empty() {
                                ui.add_space(2.0);
                                ui.horizontal_wrapped(|ui| {
                                    for (wi, w) in chips.iter().enumerate() {
                                        let on = wi == r.active_word;
                                        let base =
                                            if on { r.style.highlight } else { r.style.dim };
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
