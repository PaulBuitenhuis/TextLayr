//! Small drawing and formatting helpers shared by the screens and the
//! Settings window.
//!
//! Nothing here owns state: every item is either a pure function or a
//! stateless layout primitive, so screens can reuse them without pulling in
//! `App`.

use std::path::Path;

use crate::pipeline::ExportPhase;

use super::theme;

/// "Reveal in Finder" on macOS, "Show in folder" elsewhere.
pub(super) fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in folder"
    }
}

/// A small filled dot used for the Diagnostics status (drawn, not a glyph —
/// the bundled UI fonts have no suitable circle character).
pub(super) fn status_dot(ui: &mut egui::Ui, ok: bool) {
    let diameter = 9.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
    let color = if ok {
        theme::SUCCESS
    } else {
        ui.visuals().error_fg_color
    };
    ui.painter()
        .circle_filled(rect.center(), diameter * 0.5, color);
}

/// Seconds since the Unix epoch, 0 if the clock is before it.
pub(super) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bytes as a short human string ("148 MB", "12.3 kB").
pub(super) fn human_bytes(n: u64) -> String {
    const KB: f64 = 1000.0;
    let n = n as f64;
    if n < KB {
        return format!("{n:.0} B");
    }
    if n < KB * KB {
        return format!("{:.1} kB", n / KB);
    }
    if n < KB * KB * KB {
        return format!("{:.1} MB", n / (KB * KB));
    }
    format!("{:.2} GB", n / (KB * KB * KB))
}

/// A dismissable warning/error strip pinned to the bottom of the window.
/// Returns `true` when the user clicked Dismiss.
pub(super) fn banner(ui: &mut egui::Ui, id: &str, color: egui::Color32, text: &str) -> bool {
    let mut dismissed = false;
    egui::Panel::bottom(egui::IdSalt::new(id)).show(ui, |ui| {
        ui.add_space(3.0);
        // Dismiss is added first (right-to-left) so it always keeps its space;
        // the message then wraps into whatever is left.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::button(ui, "Dismiss").clicked() {
                dismissed = true;
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                ui.colored_label(color, "\u{26A0}");
                ui.add(egui::Label::new(egui::RichText::new(text)).wrap());
            });
        });
        ui.add_space(3.0);
    });
    dismissed
}

/// Center a block of widgets horizontally, sitting `above_center` points above
/// the optical middle of the available area.
pub(super) fn centered(ui: &mut egui::Ui, above_center: f32, add: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.available_rect_before_wrap();
    ui.allocate_ui_with_layout(
        rect.size(),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.add_space((rect.height() * 0.5 - above_center).max(24.0));
            add(ui);
        },
    );
}

/// Width of the centred column the export and done screens lay out in: wide
/// enough for the metadata line, narrow enough not to sprawl on a big window.
pub(super) const COLUMN_W: f32 = 460.0;

/// Run `add` inside a fixed-width column, centred in the space left.
///
/// Two egui details make this more than a `set_max_width` call: full-width
/// `add_space` calls widen the ui's `min_rect` (and egui never shrinks below
/// that), and a panel's own layout is `Align::Min`, so a child is placed at the
/// left however narrow it is. An explicit centred rect avoids both — and gives
/// inner `right_to_left` layouts the column's right edge to align to.
pub(super) fn column<R>(ui: &mut egui::Ui, height: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_rect_before_wrap();
    let w = COLUMN_W.min(avail.width()).max(1.0);
    let rect = egui::Rect::from_min_size(
        egui::pos2(avail.center().x - w * 0.5, avail.top()),
        egui::vec2(w, height.max(0.0)),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Center)),
        add,
    )
    .inner
}

/// A row of secondary buttons, centred as a group in the column.
///
/// The widths are measured first (as [`theme::segmented`] does) because the
/// buttons are content-sized: centring them by splitting the row in half would
/// leave the pair off-centre whenever the labels differ in length.
pub(super) fn centred_buttons(ui: &mut egui::Ui, labels: &[&str]) -> Vec<egui::Response> {
    let gap = ui.spacing().item_spacing.x;
    let widths: Vec<f32> = labels.iter().map(|l| theme::button_width(ui, l)).collect();
    let total: f32 = widths.iter().sum::<f32>() + gap * (labels.len().saturating_sub(1)) as f32;
    let mut out = Vec::with_capacity(labels.len());
    ui.horizontal(|ui| {
        ui.add_space(((COLUMN_W - total) * 0.5).max(0.0));
        for (label, w) in labels.iter().zip(&widths) {
            out.push(theme::fixed_button(ui, true, *w, label));
        }
    });
    out
}

/// A centred accent button sized to the row of secondary buttons under it,
/// then that row.
///
/// Sizing the primary action to the pair keeps the two rows reading as one
/// block; a full-column bar over two small buttons reads as two unrelated
/// things.
pub(super) fn centred_actions(
    ui: &mut egui::Ui,
    primary: &str,
    secondary: &[&str],
) -> (egui::Response, Vec<egui::Response>) {
    let gap = ui.spacing().item_spacing.x;
    let widths: Vec<f32> = secondary
        .iter()
        .map(|l| theme::button_width(ui, l))
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + gap * secondary.len().saturating_sub(1) as f32;
    let primary = ui
        .horizontal(|ui| {
            ui.add_space(((COLUMN_W - total) * 0.5).max(0.0));
            theme::primary_button_sized(ui, true, total, primary)
        })
        .inner;
    ui.add_space(10.0);
    (primary, centred_buttons(ui, secondary))
}

/// How one export pass reads in the step list.
#[derive(Clone, Copy)]
pub(super) enum Pass {
    /// Not started.
    Waiting,
    /// Running now, with its own 0..=1 fraction.
    Running(f32),
    /// Finished (so the list length stays stable while the export runs).
    Done,
}

/// One row of the export step list: a status mark, the pass name, and either its
/// own bar (while running), a detail (once done), or nothing (still to come).
pub(super) fn export_pass(ui: &mut egui::Ui, phase: ExportPhase, state: Pass, detail: &str) {
    ui.horizontal(|ui| {
        // Drawn, not glyphs: the bundled fonts have no ● / ○ / ▶.
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        let c = rect.center();
        match state {
            Pass::Done => {
                ui.painter().text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    "\u{2714}",
                    egui::FontId::proportional(14.0),
                    theme::SUCCESS,
                );
            }
            Pass::Running(_) => {
                ui.painter().circle_filled(c, 4.5, theme::ACCENT);
            }
            Pass::Waiting => {
                ui.painter().circle_stroke(
                    c,
                    4.0,
                    egui::Stroke::new(1.5, ui.visuals().weak_text_color()),
                );
            }
        }

        let name = egui::RichText::new(phase.label());
        ui.label(match state {
            Pass::Running(_) => name.strong(),
            _ => name.weak(),
        });

        let right = match state {
            Pass::Running(f) => format!("{:.0}%", f * 100.0),
            Pass::Done => detail.to_owned(),
            Pass::Waiting => String::new(),
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(right).weak().small());
        });
    });

    if let Pass::Running(f) = state {
        ui.add_space(7.0);
        ui.add(
            egui::ProgressBar::new(f)
                .desired_width(COLUMN_W)
                .corner_radius(theme::ROUND),
        );
    }
}

/// `m:ss` for a video duration.
pub(super) fn mmss(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// A loose duration for elapsed/estimated time ("48s", "2m 12s"). Deliberately
/// not a clock time, so it can't be mistaken for a deadline.
pub(super) fn rough_secs(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 60.0 {
        format!("{s:.0}s")
    } else if s < 3600.0 {
        format!("{}m {:02}s", (s / 60.0) as u64, (s as u64) % 60)
    } else {
        format!("{}h {:02}m", (s / 3600.0) as u64, ((s as u64) % 3600) / 60)
    }
}

/// The shared ingest status block: title, progress (or spinner), label, and an
/// optional cancel button. Returns whether the button was clicked.
pub(super) fn centered_block(
    ui: &mut egui::Ui,
    title: &str,
    label: &str,
    progress: Option<f32>,
    cancel: Option<&str>,
) -> bool {
    let mut clicked = false;
    centered(ui, 56.0, |ui| {
        ui.heading(title);
        ui.add_space(18.0);
        match progress {
            Some(p) => {
                ui.add(
                    egui::ProgressBar::new(p)
                        .show_percentage()
                        .desired_width(420.0)
                        .corner_radius(theme::ROUND),
                );
            }
            None => {
                ui.add(egui::Spinner::new().size(26.0));
            }
        }
        ui.add_space(12.0);
        ui.label(egui::RichText::new(label).weak());
        if let Some(text) = cancel {
            ui.add_space(18.0);
            if theme::fixed_button(ui, true, 120.0, text).clicked() {
                clicked = true;
            }
        }
    });
    clicked
}

pub(super) fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
