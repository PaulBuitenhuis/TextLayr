//! The full-window screens: drop, model install, setup, ingest, export and
//! done.
//!
//! These are the only `impl App` methods that draw a whole screen; the state
//! they read lives in `App`, and the transitions they trigger (`start_ingest`,
//! `cancel_export`, ...) stay in `mod.rs` next to `handle`, where the message
//! flow is readable in one place.

use std::sync::atomic::Ordering;

use crate::model::Model;
use crate::pipeline::ExportPhase;

use super::theme;
use super::widgets::{
    centered_block, centred_buttons, column, export_pass, file_label, human_bytes, mmss,
    reveal_label, rough_secs, Pass, COLUMN_W,
};
use super::{open_file, reveal, App, PickRequest, SUPPORT_URL};

impl App {
    pub(super) fn screen_drop(&mut self, ui: &mut egui::Ui) {
        // An install can run while dependencies are otherwise satisfied (e.g.
        // adding a second model from About), so check it before anything else.
        if self.screen_install(ui) {
            return;
        }
        if !self.deps.ok() {
            self.screen_setup(ui);
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let v = ui.visuals().clone();
        let (fill, stroke) = if self.hovering {
            (
                theme::ACCENT.gamma_multiply(0.18),
                egui::Stroke::new(2.0, theme::ACCENT),
            )
        } else {
            (
                v.faint_bg_color,
                egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
            )
        };
        let painter = ui.painter().clone();
        let radius = theme::ROUND_LG as f32;
        painter.rect_filled(rect, radius, fill);
        painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);

        let center = rect.center();
        painter.text(
            center - egui::vec2(0.0, 36.0),
            egui::Align2::CENTER_CENTER,
            "Drop a video",
            egui::FontId::proportional(30.0),
            v.strong_text_color(),
        );
        painter.text(
            center + egui::vec2(0.0, 2.0),
            egui::Align2::CENTER_CENTER,
            "Any video with a voiceover track.",
            egui::FontId::proportional(15.0),
            v.weak_text_color(),
        );
        painter.text(
            center + egui::vec2(0.0, 32.0),
            egui::Align2::CENTER_CENTER,
            "or run   textlayr <video.mp4>",
            egui::FontId::monospace(12.5),
            v.weak_text_color().gamma_multiply(0.75),
        );

        // A startup file that failed ingest: offer a retry right in the card.
        if let Some(path) = self.input.clone() {
            painter.text(
                egui::pos2(center.x, center.y + 74.0),
                egui::Align2::CENTER_CENTER,
                format!("couldn't open {}", file_label(&path)),
                egui::FontId::proportional(13.0),
                v.error_fg_color,
            );
            let btn = egui::Rect::from_center_size(
                egui::pos2(center.x - 86.0, center.y + 104.0),
                egui::vec2(120.0, theme::CONTROL_H),
            );
            let open_btn = egui::Rect::from_center_size(
                egui::pos2(center.x + 66.0, center.y + 104.0),
                egui::vec2(160.0, theme::CONTROL_H),
            );
            let clicked = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(btn)
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                    |ui| theme::fixed_button(ui, true, 120.0, "Retry"),
                )
                .inner
                .clicked();
            let open_clicked = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(open_btn)
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                    |ui| theme::fixed_button(ui, true, 160.0, "Open another\u{2026}"),
                )
                .inner
                .clicked();
            if clicked {
                self.start_ingest(path);
            }
            if open_clicked {
                self.pending_pick = Some(PickRequest::OpenVideo);
            }
        } else {
            // The windowing layer implements native file drop for X11 only, so
            // the picker is the supported way in on Wayland (and a convenience
            // everywhere else).
            let btn = egui::Rect::from_center_size(
                egui::pos2(center.x, center.y + 92.0),
                egui::vec2(180.0, theme::CONTROL_H),
            );
            let clicked = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(btn)
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                    |ui| theme::primary_button(ui, true, "Open video\u{2026}"),
                )
                .inner
                .clicked();
            if clicked {
                self.pending_pick = Some(PickRequest::OpenVideo);
            }
        }
    }

    /// Render the in-progress install, if any. Returns `true` when it drew
    /// something, so callers can skip the rest of the screen.
    pub(super) fn screen_install(&mut self, ui: &mut egui::Ui) -> bool {
        let Some((label, progress)) = self
            .install
            .as_ref()
            .map(|st| (st.label.clone(), st.progress))
        else {
            return false;
        };
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.heading("Downloading");
            ui.add_space(16.0);
        });
        ui.vertical_centered(|ui| {
            ui.set_max_width(520.0);
            match progress {
                Some(p) => {
                    ui.add(
                        egui::ProgressBar::new(p)
                            .show_percentage()
                            .desired_width(460.0)
                            .corner_radius(theme::ROUND),
                    );
                }
                None => {
                    ui.add(egui::Spinner::new().size(26.0));
                }
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new(label).weak());
            ui.add_space(14.0);
            if theme::fixed_button(ui, true, 120.0, "Cancel").clicked() {
                self.cancel_install();
            }
        });
        true
    }

    /// Shown on the drop screen when a required tool/model/font is missing.
    pub(super) fn screen_setup(&mut self, ui: &mut egui::Ui) {
        if self.screen_install(ui) {
            return;
        }
        ui.add_space(26.0);
        ui.vertical_centered(|ui| {
            ui.heading("Setup required");
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "TextLayr needs an internet connection once: it downloads ffmpeg and a \
                     whisper model (~190 MB for base.en, ~530 MB for small.en).",
                )
                .weak(),
            );
        });
        ui.add_space(14.0);

        // Model choice. Changing it re-runs the check, so the missing-items list
        // and the Install button reflect the newly selected model.
        let mut chosen = self.model_pref;
        ui.vertical_centered(|ui| {
            ui.set_max_width(560.0);
            theme::card(ui, |ui| {
                ui.label(egui::RichText::new("Transcription model").strong());
                ui.add_space(4.0);
                for m in Model::ALL {
                    ui.radio_value(&mut chosen, m, m.label());
                }
                ui.add_space(2.0);
                ui.label(egui::RichText::new("You can change this later from Settings.").weak());
            });
        });
        if chosen != self.model_pref {
            self.model_pref = chosen;
            self.persist_prefs();
            self.recheck_deps();
        }

        ui.add_space(14.0);

        let problems = self.deps.problems();
        let installable = self.deps.has_installable();
        ui.vertical_centered(|ui| {
            ui.set_max_width(560.0);
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for p in &problems {
                        theme::card(ui, |ui| {
                            ui.label(egui::RichText::new(&p.what).strong());
                            ui.add_space(2.0);
                            ui.add(egui::Label::new(egui::RichText::new(&p.how).weak()));
                        });
                        ui.add_space(8.0);
                    }
                });
        });

        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(260.0);
            if installable && theme::primary_button(ui, true, "Install").clicked() {
                self.start_install(self.model_pref);
            }
            if installable {
                ui.add_space(6.0);
            }
            if theme::button(ui, "Re-check").clicked() {
                self.recheck_deps();
            }
            if let Some(path) = self.input.clone() {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} is waiting — it will open once setup is done",
                        file_label(&path)
                    ))
                    .weak(),
                );
                ui.add_space(4.0);
                if theme::wide_button(ui, true, "Retry").clicked() {
                    self.start_ingest(path);
                }
            }
        });
    }

    pub(super) fn screen_ingest(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.ingest else { return };
        let file = file_label(&st.file);
        let label = st.label.clone();
        centered_block(ui, &file, &label, st.progress);
    }

    /// The export in progress: one row per pass, each with its own bar, plus a
    /// time estimate and a way out.
    pub(super) fn screen_export(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.export else { return };
        let file = st.file.clone();
        let phase = st.phase;
        let phase_frac = st.phase_frac;
        let states = st.states;
        let remaining = st.remaining();
        let cancelling = self
            .export_cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed));
        let mut cancel = false;

        // A fixed-width column, centred. `set_max_width` alone would be undone
        // by the full-width `add_space` calls above it (they widen `min_rect`,
        // and egui never shrinks below that), so the column is allocated to an
        // explicit size instead.
        let h = ui.available_height();
        // Sit it near the optical middle, like the ingest screen.
        ui.add_space((h * 0.5 - 118.0).max(24.0));
        column(ui, h, |ui| {
            ui.heading("Exporting");
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&file).weak());
            ui.add_space(28.0);

            for p in ExportPhase::ALL {
                let detail = match p {
                    ExportPhase::Rendering => format!("{states} states"),
                    ExportPhase::Encoding => String::new(),
                };
                let state = if p == phase {
                    Pass::Running(phase_frac)
                } else if p.before(phase) {
                    Pass::Done
                } else {
                    Pass::Waiting
                };
                export_pass(ui, p, state, &detail);
                ui.add_space(16.0);
            }

            // Always reserve the estimate line so Cancel doesn't shuffle as the
            // first estimate appears.
            ui.label(
                egui::RichText::new(match remaining {
                    Some(d) => format!("about {} remaining", rough_secs(d.as_secs_f64())),
                    None => String::new(),
                })
                .weak()
                .small(),
            );
            ui.add_space(18.0);
            if cancelling {
                ui.label(egui::RichText::new("Cancelling…").weak());
            } else if theme::fixed_button(ui, true, 120.0, "Cancel").clicked() {
                cancel = true;
            }
        });

        if cancel {
            self.cancel_export();
        }
    }

    pub(super) fn screen_done(&mut self, ui: &mut egui::Ui) {
        let Some(d) = &self.done else { return };
        let output = d.output.clone();
        let secs = d.secs;
        let (width, height, duration_secs) = (d.width, d.height, d.duration_secs);
        let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);

        // The frame the preview already has: no extra ffmpeg pass, and it has
        // the caption burned in, so the screen shows what was made rather than
        // just naming it.
        let poster = self
            .review
            .as_ref()
            .and_then(|r| r.preview.texture().zip(r.preview.image_size()));

        let mut open_video = false;
        let mut reveal_output = false;
        let mut new_video = false;
        let mut support = false;

        let avail = ui.available_size();
        // Scale the poster to the room we have, so a short window still fits the
        // whole block (there is no scroll area on this screen). Portrait video
        // is capped by height, which is why the budget is height-based.
        let poster_max_h = ((avail.y - 290.0) * 0.75).clamp(90.0, 340.0);
        let poster_size = poster.map(|(_, (iw, ih))| {
            let aspect = ih as f32 / iw.max(1) as f32;
            let w = COLUMN_W.min(poster_max_h / aspect.max(0.01));
            egui::vec2(w, w * aspect)
        });

        let block = poster_size.map(|s| s.y).unwrap_or(0.0) + 290.0;
        let top = ((avail.y - block) * 0.5).max(16.0);
        ui.add_space(top);

        column(ui, (avail.y - top).max(1.0), |ui| {
            if let (Some((tex, _)), Some(size)) = (poster, poster_size) {
                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
                ui.painter().rect_stroke(
                    rect,
                    theme::ROUND_LG as f32,
                    egui::Stroke::new(1.0, theme::BORDER),
                    egui::StrokeKind::Inside,
                );
                if resp.hovered() {
                    // A soft play affordance: dim the frame, float a disc with a
                    // hand-drawn triangle (the bundled fonts have no ▶).
                    ui.painter().rect_filled(
                        rect,
                        theme::ROUND_LG as f32,
                        egui::Color32::from_black_alpha(96),
                    );
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    let c = rect.center();
                    ui.painter()
                        .circle_filled(c, 26.0, egui::Color32::from_white_alpha(232));
                    let r = 11.0;
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(c.x - r * 0.42, c.y - r),
                            egui::pos2(c.x - r * 0.42, c.y + r),
                            egui::pos2(c.x + r * 0.78, c.y),
                        ],
                        theme::SURFACE,
                        egui::Stroke::NONE,
                    ));
                }
                if resp.clicked() {
                    open_video = true;
                }
                resp.on_hover_text("Open video");
                ui.add_space(20.0);
            }

            ui.label(
                egui::RichText::new("\u{2714}")
                    .size(15.0)
                    .color(theme::SUCCESS)
                    .strong(),
            );
            ui.add_space(2.0);
            ui.heading("Export complete");
            ui.add_space(6.0);
            ui.label(egui::RichText::new(file_label(&output)).strong())
                .on_hover_text(output.display().to_string());
            if let Some(dir) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                ui.label(
                    egui::RichText::new(dir.display().to_string())
                        .weak()
                        .small(),
                );
            }
            ui.add_space(3.0);
            ui.label(
                egui::RichText::new(format!(
                    "{width}×{height}  ·  {}  ·  {}  ·  exported in {}",
                    mmss(duration_secs),
                    human_bytes(size),
                    rough_secs(secs),
                ))
                .weak()
                .small(),
            );

            ui.add_space(22.0);
            if theme::primary_button(ui, true, "Open video").clicked() {
                open_video = true;
            }
            ui.add_space(10.0);
            // Secondary actions as real buttons, muted against the accent one
            // above and centred as a pair.
            let actions = centred_buttons(ui, &[reveal_label(), "New video"]);
            if actions.first().is_some_and(|r| r.clicked()) {
                reveal_output = true;
            }
            if actions.get(1).is_some_and(|r| r.clicked()) {
                new_video = true;
            }

            ui.add_space(24.0);
            ui.separator();
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("TextLayr is free and open source — thank you for using it.")
                    .weak()
                    .small(),
            );
            ui.add_space(8.0);
            if theme::icon_button(ui, "\u{2615}", theme::COFFEE, "Buy me a coffee").clicked() {
                support = true;
            }
        });

        if open_video {
            if let Err(e) = open_file(&output) {
                self.error = Some(format!("could not open video: {e}"));
            }
        }
        if reveal_output {
            if let Err(e) = reveal(&output) {
                self.error = Some(format!("could not open folder: {e}"));
            }
        }
        if support {
            ui.ctx().open_url(egui::OpenUrl::new_tab(SUPPORT_URL));
        }
        if new_video {
            self.reset_to_drop();
        }
    }
}
