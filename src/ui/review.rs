//! Review screen: transcript editing, style controls, live preview.

use std::path::PathBuf;

use egui::{Color32, RichText, Sense};

use crate::chunk::{self, Chunk};
use crate::config::{Color, Grouping, PopupEffect, Style};
use crate::pipeline::Ingest;
use crate::probe::{Fftools, VideoInfo};
use crate::render::MotionPhase;
use crate::transcribe::Word;

use super::preview::{FrameKey, Preview};
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
    /// Loop the popup transition in the preview.
    pub motion_playing: bool,
    pub edited_any: bool,
    pub preview: Preview,
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
            motion_playing: false,
            edited_any: false,
            preview: Preview::default(),
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

    /// Stable key for the popup's time span. Re-chunking changes the span, so a
    /// cached frame cannot be shown for different words.
    pub fn chunk_key(&self, idx: usize) -> Option<FrameKey> {
        self.chunks
            .get(idx)
            .map(|c| (c.start_secs.to_bits(), c.end_secs.to_bits()))
    }

    /// Representative timestamp for a popup's background frame. One frame per
    /// popup keeps the background stable while the highlighted word changes.
    pub fn popup_time(&self, idx: usize) -> f64 {
        self.chunks
            .get(idx)
            .map(|c| (c.start_secs + c.end_secs) / 2.0)
            .unwrap_or(0.0)
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

/// Right column: the single style inspector. Delegates to [`super::style`] so
/// the panel has one home and the review screen stays about the transcript.
pub fn style_panel(
    ui: &mut egui::Ui,
    r: &mut ReviewState,
    profiles: &crate::profile::ProfileStore,
    profile_ui: &mut super::profile::ProfileUi,
) -> Action {
    super::style::panel(ui, r, profiles, profile_ui)
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
