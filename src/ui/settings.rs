//! The Settings window: About, Models, Updates and Diagnostics.
//!
//! It is a self-contained draw pass — every action is collected into a local
//! and applied after the window closure, so the UI never re-enters `App` while
//! it is borrowed. Kept out of `mod.rs` because it is the one screen that is
//! large and rarely touched.

use std::path::PathBuf;

use crate::config;
use crate::model::{FoundModel, Model};
use crate::update;

use super::theme;
use super::widgets::{human_bytes, reveal_label};
use super::{
    diagnostic_entry, reveal, App, UpdateUi, DEVELOPER, DEVELOPER_URL, REPO_URL, SUPPORT_URL,
    TAGLINE,
};

/// The selected tab in the About window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AboutTab {
    About,
    Models,
    Updates,
    Diagnostics,
}

impl AboutTab {
    const ALL: [AboutTab; 4] = [
        AboutTab::About,
        AboutTab::Models,
        AboutTab::Updates,
        AboutTab::Diagnostics,
    ];

    fn label(self) -> &'static str {
        match self {
            AboutTab::About => "About",
            AboutTab::Models => "Models",
            AboutTab::Updates => "Updates",
            AboutTab::Diagnostics => "Diagnostics",
        }
    }
}

impl App {
    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        let mut open_licenses = false;
        let mut buy_coffee = false;
        let mut reveal_path: Option<PathBuf> = None;
        let mut remove_data_now = false;

        // Actions are collected during the draw and applied after the window
        // closure, so the UI never re-enters `self` while it is borrowed.
        let update_state = self.update.clone();
        let auto_check = self.saved_prefs.auto_check_updates;
        let mut check_updates = false;
        let mut start_install_update: Option<update::Release> = None;
        let mut cancel_install_update = false;
        let mut set_auto_check: Option<bool> = None;

        // Bound the panel to the viewport so a short display can't clip it.
        let screen_h = ctx.input(|i| i.viewport_rect().height());
        let max_h = (screen_h - 80.0).clamp(420.0, 660.0);
        let mut tab_idx = AboutTab::ALL
            .iter()
            .position(|t| *t == self.about_tab)
            .unwrap_or(0);

        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .fixed_size(egui::vec2(500.0, max_h))
            .show(ctx, |ui| {
                // Take the width the window actually offers, rather than a
                // hard-coded 476. The window is `fixed_size(500)` *including*
                // its 12px margin and 1px stroke, so the content box is 474
                // wide: asking for 476 made the body overflow to the right,
                // and egui paints the title band from the declared window size
                // while the body followed the overflow — leaving a 2px sliver
                // between the title band and the right edge of the window.
                ui.set_width(ui.available_width());

                // ── tab strip ─────────────────────────────────────────────
                let labels: Vec<&str> = AboutTab::ALL.iter().map(|t| t.label()).collect();
                if theme::segmented(ui, &mut tab_idx, &labels) {
                    self.about_tab = AboutTab::ALL[tab_idx];
                }
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                // ── footer pinned to the bottom (Mac-panel feel) ──────────
                egui::Panel::bottom("about_footer")
                    .show_separator_line(false)
                    .show(ui, |ui| {
                        // Draw the rule ourselves so there's exactly one.
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);
                        ui.vertical_centered(|ui| {
                            if theme::fixed_button(ui, true, 120.0, "Close").clicked() {
                                close = true;
                            }
                        });
                        ui.add_space(6.0);
                    });

                // ── tab body (scrolls only if a tab overflows) ────────────
                egui::ScrollArea::vertical()
                    .id_salt("about_body")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let tab = self.about_tab;
                        match tab {
                            AboutTab::About => {
                                ui.vertical_centered(|ui| {
                                    // The wordmark, exactly as on the main screen.
                                    match self.banner.as_ref() {
                                        Some(banner) => {
                                            let w = 232.0;
                                            let h = w * banner.size()[1] as f32
                                                / banner.size()[0] as f32;
                                            ui.add(
                                                egui::Image::new(banner)
                                                    .fit_to_exact_size(egui::vec2(w, h)),
                                            );
                                        }
                                        None => {
                                            ui.label(
                                                egui::RichText::new("TextLayr").size(22.0).strong(),
                                            );
                                        }
                                    }
                                    ui.add_space(10.0);
                                    ui.label(egui::RichText::new(TAGLINE).weak());
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Version {}   ·   build {}",
                                            env!("CARGO_PKG_VERSION"),
                                            env!("TEXTLAYR_GIT_HASH")
                                        ))
                                        .weak(),
                                    );

                                    ui.add_space(12.0);

                                    // "Made with ❤ by Paul Buitenhuis", heart tinted warm.
                                    let mut job = egui::text::LayoutJob {
                                        halign: egui::Align::Center,
                                        ..Default::default()
                                    };
                                    let fmt = |color: egui::Color32| egui::TextFormat {
                                        font_id: egui::FontId::proportional(13.5),
                                        color,
                                        ..Default::default()
                                    };
                                    let ink = ui.visuals().text_color();
                                    let tail = format!(" by {DEVELOPER}");
                                    job.append("Made with ", 0.0, fmt(ink));
                                    job.append("\u{2764}", 0.0, fmt(theme::HEART));
                                    job.append(&tail, 0.0, fmt(ink));
                                    ui.label(job);

                                    ui.add_space(6.0);

                                    // Links, centred by hand (no centered horizontal).
                                    ui.horizontal(|ui| {
                                        ui.add_space((ui.available_width() - 188.0).max(0.0) * 0.5);
                                        ui.hyperlink_to("GitHub", DEVELOPER_URL);
                                        ui.label("·");
                                        ui.hyperlink_to("Source", REPO_URL);
                                        ui.label("·");
                                        ui.label("MIT licensed");
                                    });

                                    ui.add_space(8.0);
                                    if theme::button(ui, "Open source licenses").clicked() {
                                        open_licenses = true;
                                    }
                                });

                                ui.add_space(14.0);
                                // Support lives in a card so it reads as a
                                // deliberate section, not a footnote.
                                theme::card(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.label(egui::RichText::new("Support TextLayr").strong());
                                        ui.add_space(6.0);

                                        // A LayoutJob so both wrapped lines centre,
                                        // not just the block.
                                        let mut job = egui::text::LayoutJob {
                                            halign: egui::Align::Center,
                                            ..Default::default()
                                        };
                                        job.append(
                                            "TextLayr is free, and it stays free. There's no \
                                             company behind it — just me, making it in \
                                             whatever time I can find. If it saved you some \
                                             time, a coffee is a nice way to say thanks.",
                                            0.0,
                                            egui::TextFormat {
                                                font_id: egui::FontId::proportional(13.5),
                                                color: ui.visuals().weak_text_color(),
                                                ..Default::default()
                                            },
                                        );
                                        ui.label(job);

                                        ui.add_space(12.0);
                                        if theme::icon_button(
                                            ui,
                                            "\u{2615}",
                                            theme::COFFEE,
                                            "Buy me a coffee",
                                        )
                                        .clicked()
                                        {
                                            buy_coffee = true;
                                        }
                                    });
                                });
                            }
                            AboutTab::Models => {
                                // What's installed, which is in use, and how to
                                // change it. Each model is its own block with its
                                // own actions — "Use" switches to an installed one.
                                let in_use = self.deps.model.as_ref().map(|f| f.model);
                                let models: Vec<(Model, Option<FoundModel>)> = Model::ALL
                                    .iter()
                                    .map(|m| (*m, crate::model::locate(*m)))
                                    .collect();
                                let pending = self.pending_remove;
                                let installing = self
                                    .install
                                    .as_ref()
                                    .map(|st| (st.label.clone(), st.progress));
                                let install_target = self.install_target;
                                let mut chosen = self.model_pref;
                                let mut download: Option<Model> = None;
                                let mut remove: Option<FoundModel> = None;
                                let mut set_pending: Option<Option<Model>> = None;
                                let mut cancel_install = false;

                                ui.label(egui::RichText::new("Transcription model").strong());
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(
                                        "TextLayr transcribes locally with Whisper. A bigger \
                                         model is slower but more accurate.",
                                    )
                                    .weak(),
                                );
                                ui.add_space(10.0);

                                // An in-flight download gets its own card, so it
                                // is obvious and can be cancelled without leaving.
                                if let Some((label, progress)) = &installing {
                                    theme::card(ui, |ui| {
                                        ui.label(egui::RichText::new(label.as_str()).weak());
                                        ui.add_space(8.0);
                                        match progress {
                                            Some(p) => {
                                                ui.add(
                                                    egui::ProgressBar::new(*p)
                                                        .show_percentage()
                                                        .desired_width(ui.available_width())
                                                        .corner_radius(theme::ROUND),
                                                );
                                            }
                                            None => {
                                                ui.add(egui::Spinner::new().size(20.0));
                                            }
                                        }
                                        ui.add_space(10.0);
                                        if theme::button(ui, "Cancel download").clicked() {
                                            cancel_install = true;
                                        }
                                    });
                                    ui.add_space(10.0);
                                }

                                for (m, found) in &models {
                                    // Each model is a self-contained block: its name,
                                    // its status, then its own buttons directly
                                    // beneath — so no action can be mistaken for
                                    // another model's.
                                    ui.label(egui::RichText::new(m.label()).strong());
                                    match found {
                                        Some(f) => {
                                            let active = in_use == Some(*m);
                                            if active {
                                                ui.label(
                                                    egui::RichText::new("Active model")
                                                        .color(theme::ACCENT)
                                                        .small(),
                                                )
                                                .on_hover_text(f.path.display().to_string());
                                            } else {
                                                ui.label(
                                                    egui::RichText::new(f.source.label())
                                                        .weak()
                                                        .small(),
                                                )
                                                .on_hover_text(f.path.display().to_string());
                                            }
                                            ui.add_space(6.0);
                                            ui.horizontal(|ui| {
                                                if install_target == Some(*m)
                                                    && installing.is_some()
                                                {
                                                    ui.label(
                                                        egui::RichText::new("Downloading…")
                                                            .color(theme::ACCENT)
                                                            .small(),
                                                    );
                                                } else if pending == Some(*m) {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "Remove {}?",
                                                            m.id()
                                                        ))
                                                        .weak()
                                                        .small(),
                                                    );
                                                    if theme::button(ui, "Remove").clicked() {
                                                        remove = Some(f.clone());
                                                    }
                                                    if theme::button(ui, "Keep").clicked() {
                                                        set_pending = Some(None);
                                                    }
                                                } else {
                                                    if !active && theme::button(ui, "Use").clicked()
                                                    {
                                                        chosen = *m;
                                                    }
                                                    if f.source.removable() {
                                                        if theme::button(ui, "Remove").clicked() {
                                                            set_pending = Some(Some(*m));
                                                        }
                                                    } else if theme::button(ui, reveal_label())
                                                        .on_hover_text(
                                                            "TextLayr only deletes models it \
                                                             downloaded — reveal this one to \
                                                             remove it yourself.",
                                                        )
                                                        .clicked()
                                                    {
                                                        reveal_path = Some(f.path.clone());
                                                    }
                                                }
                                            });
                                        }
                                        None => {
                                            ui.label(
                                                egui::RichText::new("Not installed").weak().small(),
                                            );
                                            ui.add_space(6.0);
                                            ui.horizontal(|ui| {
                                                if install_target == Some(*m)
                                                    && installing.is_some()
                                                {
                                                    ui.label(
                                                        egui::RichText::new("Downloading…")
                                                            .color(theme::ACCENT)
                                                            .small(),
                                                    );
                                                } else if theme::button(ui, "Download").clicked() {
                                                    download = Some(*m);
                                                }
                                            });
                                        }
                                    }
                                    ui.add_space(18.0);
                                }
                                // No model at all: make that explicit rather
                                // than looking fine.
                                if models.iter().all(|(_, f)| f.is_none()) {
                                    ui.label(
                                        egui::RichText::new(
                                            "No model installed yet — download one to start.",
                                        )
                                        .color(ui.visuals().warn_fg_color)
                                        .small(),
                                    );
                                }

                                if let Some(Some(m)) = set_pending {
                                    self.pending_remove = Some(m);
                                } else if set_pending.is_some() {
                                    self.pending_remove = None;
                                }
                                if let Some(f) = remove {
                                    self.remove_model(&f);
                                }
                                if cancel_install {
                                    self.cancel_install();
                                }
                                if let Some(m) = download {
                                    // Stay in the window; progress shows inline.
                                    self.start_install(m);
                                } else if chosen != self.model_pref {
                                    self.pending_remove = None;
                                    self.model_pref = chosen;
                                    self.persist_prefs();
                                    self.recheck_deps();
                                }
                            }
                            AboutTab::Updates => {
                                // Check for a newer release, download & install
                                // in place, and the auto-check preference.
                                match &update_state {
                                    UpdateUi::Unknown => {
                                        ui.label(egui::RichText::new("Not checked yet.").weak());
                                    }
                                    UpdateUi::Checking => {
                                        ui.horizontal(|ui| {
                                            ui.add(egui::Spinner::new().size(14.0));
                                            ui.label(
                                                egui::RichText::new("Checking for updates…").weak(),
                                            );
                                        });
                                    }
                                    UpdateUi::UpToDate => {
                                        ui.label(
                                            egui::RichText::new("TextLayr is up to date.").weak(),
                                        );
                                    }
                                    UpdateUi::Available(r) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Version {} is available.",
                                                r.version
                                            ))
                                            .color(theme::ACCENT),
                                        );
                                        ui.add_space(6.0);
                                        ui.horizontal(|ui| {
                                            if r.installable() {
                                                // Linux has no in-place update yet:
                                                // the package is downloaded and
                                                // handed over, so don't promise a
                                                // restart.
                                                let label = if cfg!(target_os = "linux") {
                                                    "Download package"
                                                } else {
                                                    "Download & install"
                                                };
                                                if theme::button(ui, label).clicked() {
                                                    start_install_update = Some(r.clone());
                                                }
                                            } else {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "No package for this platform yet.",
                                                    )
                                                    .weak(),
                                                );
                                            }
                                            ui.hyperlink_to("Release notes", &r.notes_url);
                                        });
                                    }
                                    UpdateUi::Installing { label, progress } => {
                                        match progress {
                                            Some(p) => {
                                                ui.add(
                                                    egui::ProgressBar::new(*p)
                                                        .show_percentage()
                                                        .desired_width(ui.available_width())
                                                        .corner_radius(theme::ROUND),
                                                );
                                            }
                                            None => {
                                                ui.add(egui::Spinner::new().size(20.0));
                                            }
                                        }
                                        ui.add_space(4.0);
                                        ui.label(egui::RichText::new(label.as_str()).weak());
                                        if theme::button(ui, "Cancel").clicked() {
                                            cancel_install_update = true;
                                        }
                                    }
                                    UpdateUi::Ready => {
                                        ui.label(
                                            egui::RichText::new("Restarting to finish the update…")
                                                .weak(),
                                        );
                                    }
                                    UpdateUi::OpenedDmg => {
                                        let text = if cfg!(target_os = "linux") {
                                            "The package was downloaded and shown in your file \
                                             manager. Install it by hand — this copy keeps running."
                                        } else {
                                            "The installer is open — drag TextLayr into \
                                             Applications, then quit and reopen it."
                                        };
                                        ui.label(egui::RichText::new(text).weak());
                                    }
                                    UpdateUi::Failed(e) => {
                                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                                    }
                                }

                                ui.add_space(8.0);
                                let busy = matches!(
                                    &update_state,
                                    UpdateUi::Checking
                                        | UpdateUi::Installing { .. }
                                        | UpdateUi::Ready
                                );
                                ui.horizontal(|ui| {
                                    if theme::button_enabled(ui, !busy, "Check for updates")
                                        .clicked()
                                    {
                                        check_updates = true;
                                    }
                                    let mut auto = auto_check;
                                    if ui
                                        .checkbox(&mut auto, "Check automatically")
                                        .on_hover_text(
                                            "Look for a new version once a day when TextLayr \
                                             starts.",
                                        )
                                        .changed()
                                    {
                                        set_auto_check = Some(auto);
                                    }
                                });
                            }
                            AboutTab::Diagnostics => {
                                ui.label(
                                    egui::RichText::new(
                                        "TextLayr uses the files below. If something isn't \
                                         working, check that each one exists — or copy this \
                                         report into a bug report.",
                                    )
                                    .weak(),
                                );
                                ui.add_space(12.0);

                                let fftools = self.deps.fftools.as_ref();
                                let ffmpeg = fftools.map(|t| (t.ffmpeg.clone(), t.source));
                                let ffprobe = fftools.map(|t| (t.ffprobe.clone(), t.source));
                                let model =
                                    self.deps.model.as_ref().map(|m| (m.path.clone(), m.source));
                                let fonts = self.deps.fonts.clone();
                                let config = config::config_path();

                                let entries = [
                                    (
                                        "ffmpeg",
                                        ffmpeg
                                            .as_ref()
                                            .map(|(p, s)| (p.as_path(), Some(s.label()))),
                                    ),
                                    (
                                        "ffprobe",
                                        ffprobe
                                            .as_ref()
                                            .map(|(p, s)| (p.as_path(), Some(s.label()))),
                                    ),
                                    (
                                        "model",
                                        model.as_ref().map(|(p, s)| (p.as_path(), Some(s.label()))),
                                    ),
                                    (
                                        "fonts",
                                        fonts.as_ref().map(|(p, s)| (p.as_path(), Some(s.label()))),
                                    ),
                                    ("config", config.as_deref().map(|p| (p, None))),
                                ];
                                for (label, value) in entries {
                                    if let Some(p) = diagnostic_entry(ui, label, value) {
                                        reveal_path = Some(p);
                                    }
                                }

                                ui.add_space(4.0);
                                if theme::button(ui, "Copy diagnostics report").clicked() {
                                    ui.ctx().copy_text(self.copy_diagnostics_report());
                                }

                                ui.add_space(16.0);
                                ui.separator();
                                ui.add_space(10.0);
                                let data_size = crate::install::downloaded_size();
                                let busy = self.ingest.is_some()
                                    || self.export.is_some()
                                    || self.install.is_some();
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("Downloaded dependencies").strong(),
                                    );
                                    ui.label(egui::RichText::new(human_bytes(data_size)).weak());
                                });
                                ui.label(
                                    egui::RichText::new(
                                        "ffmpeg, ffprobe and the whisper models. Safe to remove \
                                         any time — TextLayr downloads what it needs again.",
                                    )
                                    .weak()
                                    .small(),
                                );
                                ui.add_space(6.0);
                                if data_size > 0 && busy {
                                    ui.label(
                                        egui::RichText::new(
                                            "Finish the current video before removing these.",
                                        )
                                        .weak()
                                        .small(),
                                    );
                                } else if data_size > 0 {
                                    if self.pending_remove_data {
                                        ui.label(
                                            egui::RichText::new(
                                                "Remove them? You'll be asked to download again \
                                                 before the next video.",
                                            )
                                            .small(),
                                        );
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            if theme::primary_button(ui, true, "Remove").clicked() {
                                                remove_data_now = true;
                                            }
                                            if theme::button(ui, "Cancel").clicked() {
                                                self.pending_remove_data = false;
                                            }
                                        });
                                    } else if theme::button(
                                        ui,
                                        "Remove downloaded models and ffmpeg…",
                                    )
                                    .clicked()
                                    {
                                        self.pending_remove_data = true;
                                    }
                                }
                            }
                        }
                    });
            });

        if open_licenses {
            self.licenses_open = true;
        }
        if buy_coffee {
            ctx.open_url(egui::OpenUrl::new_tab(SUPPORT_URL));
        }
        if let Some(p) = reveal_path {
            if let Err(e) = reveal(&p) {
                self.error = Some(format!("could not open folder: {e}"));
            }
        }
        if remove_data_now {
            match crate::install::remove_downloaded() {
                Ok(()) => {
                    self.pending_remove_data = false;
                    self.recheck_deps();
                }
                Err(e) => {
                    self.pending_remove_data = false;
                    self.error = Some(format!("{e:#}"));
                }
            }
        }
        // Apply the update actions collected during the draw.
        if let Some(auto) = set_auto_check {
            self.saved_prefs.auto_check_updates = auto;
            self.persist_prefs();
        }
        if check_updates {
            self.start_update_check(true);
        }
        if let Some(release) = start_install_update {
            self.start_update_install(release);
        }
        if cancel_install_update {
            self.cancel_update_install();
        }
        if !open || close {
            self.about_open = false;
        }
    }
}
