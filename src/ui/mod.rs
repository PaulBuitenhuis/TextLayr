//! The egui front-end: drop → ingest (progress) → review → export → done.
//!
//! All slow work (whisper, ffmpeg) runs on worker threads and reports through
//! a channel polled every UI frame; the UI thread never blocks.

mod preview;
mod review;
mod theme;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{self, Style};
use crate::deps;
use crate::pipeline::{self, ExportProgress, ExportSink, IngestProgress, Progress};
use preview::Frame;
use review::{Action, ReviewState};

/// Where the app is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Drop,
    Ingest,
    Review,
    Export,
    Done,
}

struct IngestStatus {
    file: PathBuf,
    label: String,
    /// `None` = indeterminate (spinner) phase.
    progress: Option<f32>,
}

struct ExportStatus {
    label: String,
    progress: f32,
}

struct DoneInfo {
    output: PathBuf,
    secs: f64,
}

/// Messages from worker threads.
enum Msg {
    Ingest(IngestProgress),
    IngestOk(Box<pipeline::Ingest>),
    IngestErr(String),
    Frame {
        token: u64,
        image: Option<Frame>,
    },
    Export(ExportProgress),
    ExportOk {
        output: PathBuf,
        secs: f64,
    },
    ExportErr(String),
}

pub struct App {
    stage: Stage,
    /// Video to ingest: CLI argument at startup, or a dropped file.
    input: Option<PathBuf>,
    ingest: Option<IngestStatus>,
    review: Option<ReviewState>,
    export: Option<ExportStatus>,
    done: Option<DoneInfo>,
    error: Option<String>,
    warn: Option<String>,
    /// A file is being hovered over the drop zone.
    hovering: bool,
    /// Startup dependency check (ffmpeg, model, fonts).
    deps: deps::Check,
    /// The style currently persisted on disk; a change from the live style
    /// schedules an autosave.
    saved_style: Style,
    /// When the pending style autosave may fire (debounce).
    style_save_at: Option<Instant>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Outstanding worker messages (drives the idle repoll).
    pending: usize,
    /// Generation counter so stale frame extractions are dropped.
    frame_token: u64,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let saved_style = config::load_style();
        let deps = deps::Check::run();
        let mut app = App {
            stage: Stage::Drop,
            input: None,
            ingest: None,
            review: None,
            export: None,
            done: None,
            error: None,
            warn: None,
            hovering: false,
            deps,
            saved_style,
            style_save_at: None,
            tx,
            rx,
            pending: 0,
            frame_token: 0,
        };
        if let Some(path) = initial {
            if !path.is_file() {
                app.error = Some(format!("input file not found: {}", path.display()));
            } else if app.deps.ok() {
                app.start_ingest(path);
            } else {
                // Dependencies are missing; keep the file so "Re-check" can
                // resume once they're installed.
                app.input = Some(path);
            }
        }
        app
    }

    /// Re-run the dependency check (the user may have installed something
    /// since launch) and clear the banner if everything is now present.
    fn recheck_deps(&mut self) {
        self.deps = deps::Check::run();
        if self.deps.ok() {
            self.error = None;
        }
    }

    /// Debounced autosave of the review style. Called once per frame; writes
    /// the config only after the style has been stable for a beat, so dragging
    /// a slider doesn't hammer the disk.
    fn autosave_style(&mut self, ctx: &egui::Context) {
        const DEBOUNCE: Duration = Duration::from_millis(400);
        let dirty = match &self.review {
            Some(r) if r.style != self.saved_style => Some(r.style.clone()),
            _ => None,
        };
        let Some(style) = dirty else {
            self.style_save_at = None;
            return;
        };
        let now = Instant::now();
        let deadline = *self.style_save_at.get_or_insert(now + DEBOUNCE);
        if now >= deadline {
            self.saved_style = style;
            self.style_save_at = None;
            if let Err(e) = config::save_style(&self.saved_style) {
                eprintln!("      style save failed: {e:#}");
            }
        } else {
            // Make sure a frame runs at the deadline even if the user stops
            // interacting (egui otherwise sleeps).
            ctx.request_repaint_after(deadline - now);
        }
    }

    // ── workers ───────────────────────────────────────────────────────────

    fn start_ingest(&mut self, path: PathBuf) {
        self.error = None;
        self.warn = None;
        // Refuse to start against a broken environment; the drop screen shows
        // exactly what is missing and a "Re-check" button.
        if !self.deps.ok() {
            self.recheck_deps();
            self.input = Some(path);
            self.error = self.deps.summary();
            return;
        }
        self.input = Some(path.clone());
        self.ingest = Some(IngestStatus {
            file: path.clone(),
            label: "starting…".into(),
            progress: None,
        });
        self.stage = Stage::Ingest;

        let tx = self.tx.clone();
        let done_tx = tx.clone();
        let sink: Progress = Box::new(move |ev| {
            let _ = tx.send(Msg::Ingest(ev));
        });
        self.pending += 1;
        std::thread::spawn(move || {
            let msg = match pipeline::ingest(&path, false, sink) {
                Ok(ing) => Msg::IngestOk(Box::new(ing)),
                Err(e) => Msg::IngestErr(format!("{e:#}")),
            };
            let _ = done_tx.send(msg);
        });
    }

    fn seek_preview(&mut self) {
        let Some(r) = self.review.as_mut() else { return };
        r.preview.invalidate();
        let key = (r.selected, r.active_word);
        if r.frame_key == key {
            return;
        }
        r.frame_key = key;
        let at = r.preview_time();
        let tools = r.tools.clone();
        let input = r.input.clone();

        self.frame_token += 1;
        let token = self.frame_token;
        let tx = self.tx.clone();
        self.pending += 1;
        std::thread::spawn(move || {
            let image = preview::extract(&tools, &input, at).ok();
            let _ = tx.send(Msg::Frame { token, image });
        });
    }

    fn start_export(&mut self) {
        if self.stage != Stage::Review {
            eprintln!("      export ignored: stage is not review");
            return;
        }
        let Some(r) = self.review.as_ref() else {
            eprintln!("      export ignored: no review state");
            return;
        };
        let chunks = r.to_chunks();
        if chunks.is_empty() {
            self.error = Some("nothing to export: every caption is empty".into());
            return;
        }
        let input = r.input.clone();
        let tools = r.tools.clone();
        let info = r.info.clone();
        let style = r.style.clone();
        let output = pipeline::default_output(&input);

        self.error = None;
        self.export = Some(ExportStatus {
            label: "starting…".into(),
            progress: 0.0,
        });
        self.stage = Stage::Export;

        let tx = self.tx.clone();
        let done_tx = tx.clone();
        let sink: ExportSink = Box::new(move |p| {
            let _ = tx.send(Msg::Export(p));
        });
        self.pending += 1;
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            match pipeline::export_video(
                &tools,
                &input,
                &output,
                &info,
                &chunks,
                &style,
                crate::export::Encoder::Auto,
                sink,
            ) {
                Ok(()) => {
                    let _ = done_tx.send(Msg::ExportOk {
                        output,
                        secs: started.elapsed().as_secs_f64(),
                    });
                }
                Err(e) => {
                    let _ = done_tx.send(Msg::ExportErr(format!("{e:#}")));
                }
            }
        });
    }

    fn reset_to_drop(&mut self) {
        self.stage = Stage::Drop;
        self.input = None;
        self.ingest = None;
        self.review = None;
        self.export = None;
        self.done = None;
        self.error = None;
        self.warn = None;
        // Drop any in-flight frame extraction.
        self.frame_token = u64::MAX;
    }

    // ── message pump ──────────────────────────────────────────────────────

    fn poll(&mut self, ctx: &egui::Context) {
        let mut woke = false;
        while let Ok(msg) = self.rx.try_recv() {
            woke = true;
            self.handle(msg);
        }
        if woke {
            ctx.request_repaint();
        }
        if self.pending > 0 {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Ingest(p) => {
                if let Some(st) = &mut self.ingest {
                    match p {
                        IngestProgress::Tools(_) => st.label = "locating ffmpeg…".into(),
                        IngestProgress::Probing => st.label = "probing video…".into(),
                        IngestProgress::Probed(info) => {
                            st.label = format!(
                                "{}×{} · {:.1}s · {:.2} fps{}",
                                info.display_width,
                                info.display_height,
                                info.duration_secs,
                                info.fps,
                                if info.pix_fmt.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {}", info.pix_fmt)
                                }
                            );
                        }
                        IngestProgress::Cached(n) => {
                            st.label = format!("{n} words loaded from cache");
                            st.progress = Some(1.0);
                        }
                        IngestProgress::Extracting => {
                            st.label = "extracting audio…".into();
                            st.progress = None;
                        }
                        IngestProgress::Transcribing(f) => {
                            st.label = "transcribing with whisper small.en…".into();
                            st.progress = Some(f);
                        }
                        IngestProgress::Transcribed {
                            words,
                            secs,
                            realtime_x,
                        } => {
                            st.label =
                                format!("{words} words in {secs:.1}s ({realtime_x:.1}× realtime)");
                            st.progress = Some(1.0);
                        }
                    }
                }
            }
            Msg::IngestOk(ing) => {
                self.pending = self.pending.saturating_sub(1);
                let input = self.input.clone().unwrap_or_default();
                let style = self.saved_style.clone();
                self.review = Some(ReviewState::new(input, *ing, style));
                self.ingest = None;
                self.stage = Stage::Review;
                self.seek_preview();
            }
            Msg::IngestErr(e) => {
                self.pending = self.pending.saturating_sub(1);
                self.ingest = None;
                self.error = Some(e);
                self.stage = Stage::Drop;
            }
            Msg::Frame { token, image } => {
                self.pending = self.pending.saturating_sub(1);
                if token != self.frame_token || self.stage != Stage::Review {
                    return;
                }
                match image {
                    Some(frame) => {
                        if let Some(r) = self.review.as_mut() {
                            r.preview.set_frame(frame);
                        }
                    }
                    None => self.error = Some("frame extraction failed".into()),
                }
            }
            Msg::Export(p) => {
                if let Some(st) = &mut self.export {
                    match p {
                        ExportProgress::Rendering(f) => {
                            st.label = "rendering caption states…".into();
                            st.progress = f;
                        }
                        ExportProgress::Encoding(f) => {
                            st.label = "encoding with ffmpeg…".into();
                            st.progress = f;
                        }
                    }
                }
            }
            Msg::ExportOk { output, secs } => {
                self.pending = self.pending.saturating_sub(1);
                self.export = None;
                self.done = Some(DoneInfo { output, secs });
                self.stage = Stage::Done;
            }
            Msg::ExportErr(e) => {
                self.pending = self.pending.saturating_sub(1);
                self.export = None;
                // The banner shows this to the user; stderr is the only place
                // a headless harness (or a terminal user) can see it too.
                eprintln!("      export failed: {e}");
                self.error = Some(e);
                self.stage = Stage::Review;
            }
        }
    }

    // ── input ─────────────────────────────────────────────────────────────

    fn handle_drops(&mut self, ctx: &egui::Context) {
        // egui 0.36: dropped files are backend handles behind the `DroppedFile`
        // trait on `raw`; paths are absolute on native platforms.
        let (dropped, hovered) = ctx.input(|i| {
            let paths: Vec<PathBuf> = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect();
            (paths, !i.raw.hovered_files.is_empty())
        });
        self.hovering = hovered && self.stage == Stage::Drop;
        if self.stage != Stage::Drop {
            return;
        }
        if let Some(path) = dropped.into_iter().next() {
            if path.is_file() {
                self.start_ingest(path);
            } else {
                self.error = Some(format!("not a file: {}", path.display()));
            }
        }
    }

    // ── screens ───────────────────────────────────────────────────────────

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("texxy").size(17.0).strong());
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(2.0);
            match &self.stage {
                Stage::Drop => {
                    ui.label(egui::RichText::new("offline karaoke caption burner").weak());
                }
                Stage::Ingest => {
                    if let Some(st) = &self.ingest {
                        ui.label(file_label(&st.file));
                    }
                }
                Stage::Review => {
                    if let Some(r) = &self.review {
                        ui.label(file_label(&r.input));
                        ui.label(
                            egui::RichText::new(format!("· {} popups", r.chunks.len())).weak(),
                        );
                    }
                }
                Stage::Export => {
                    ui.label(egui::RichText::new("exporting…").weak());
                }
                Stage::Done => {
                    ui.label(egui::RichText::new("export complete").weak());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.stage == Stage::Review && theme::button(ui, "Change video").clicked() {
                    self.reset_to_drop();
                }
            });
        });
        ui.add_space(1.0);
    }

    fn screen_drop(&mut self, ui: &mut egui::Ui) {
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
            "or run   texxy <video.mp4>",
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
                egui::pos2(center.x, center.y + 104.0),
                egui::vec2(120.0, theme::CONTROL_H),
            );
            let retry = egui::Button::new("Retry").min_size(egui::vec2(120.0, theme::CONTROL_H));
            if ui.put(btn, retry).clicked() {
                self.start_ingest(path);
            }
        }

        // Build stamp, tucked away in the corner of the home screen.
        painter.text(
            rect.right_bottom() - egui::vec2(12.0, 10.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("build {}", env!("TEXXY_GIT_HASH")),
            egui::FontId::proportional(10.5),
            v.weak_text_color().gamma_multiply(0.6),
        );
    }

    /// Shown on the drop screen when a required tool/model/font is missing.
    fn screen_setup(&mut self, ui: &mut egui::Ui) {
        ui.add_space(32.0);
        ui.vertical_centered(|ui| {
            ui.heading("Setup required");
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("texxy needs these before it can caption a video:").weak(),
            );
        });
        ui.add_space(16.0);

        let problems = self.deps.problems();
        ui.vertical_centered(|ui| {
            ui.set_max_width(560.0);
            egui::ScrollArea::vertical()
                .max_height(320.0)
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

        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(240.0);
            if theme::primary_button(ui, true, "Re-check").clicked() {
                self.recheck_deps();
            }
            if let Some(path) = self.input.clone() {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} is waiting — open it again once setup is done",
                        file_label(&path)
                    ))
                    .weak(),
                );
                ui.add_space(4.0);
                if theme::button(ui, "Retry").clicked() {
                    self.start_ingest(path);
                }
            }
        });
    }

    fn screen_ingest(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.ingest else { return };
        let file = file_label(&st.file);
        let label = st.label.clone();
        centered_block(ui, &file, &label, st.progress);
    }

    fn screen_export(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.export else { return };
        let label = st.label.clone();
        centered_block(ui, "Exporting", &label, Some(st.progress));
    }

    fn screen_done(&mut self, ui: &mut egui::Ui) {
        let Some(d) = &self.done else { return };
        let output = d.output.clone();
        let secs = d.secs;
        let size_mb = std::fs::metadata(&output)
            .map(|m| m.len() as f64 / 1e6)
            .unwrap_or(0.0);

        centered(ui, 88.0, |ui| {
            ui.heading("Export complete");
            ui.add_space(8.0);
            ui.label(egui::RichText::new(file_label(&output)).strong());
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("{size_mb:.1} MB · took {secs:.0}s")).weak(),
            );
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                ui.set_max_width(260.0);
                let reveal_label = if cfg!(target_os = "macos") {
                    "Reveal in Finder"
                } else {
                    "Show in folder"
                };
                if theme::primary_button(ui, true, reveal_label).clicked() {
                    if let Err(e) = reveal(&output) {
                        self.error = Some(format!("could not open folder: {e}"));
                    }
                }
                ui.add_space(8.0);
                if theme::button(ui, "New video").clicked() {
                    self.reset_to_drop();
                }
            });
        });
    }

    // ── review layout ─────────────────────────────────────────────────────

    fn review_stage(&mut self, ui: &mut egui::Ui) {
        let mut seek = false;
        let mut export = false;
        let mut rechunk_warn = false;

        let merge = |a: Action, seek: &mut bool, export: &mut bool, rechunk: &mut bool| {
            match a {
                Action::None => {}
                Action::Seek => *seek = true,
                Action::Export => *export = true,
                Action::Rechunked { discarded_edits } => {
                    *seek = true;
                    *rechunk = *rechunk || discarded_edits;
                }
            }
        };

        // Ctrl+E (Cmd on macOS) exports from anywhere in review. Consumed at
        // the app level so a focused text editor never sees the key.
        if ui
            .ctx()
            .input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::E))
        {
            export = true;
        }

        if let Some(r) = self.review.as_mut() {
            egui::Panel::left("preview_panel")
                .resizable(true)
                .default_size(430.0)
                .min_size(300.0)
                .show(ui, |ui| {
                    let a = review::preview_panel(ui, r);
                    merge(a, &mut seek, &mut export, &mut rechunk_warn);
                });
        }
        if let Some(r) = self.review.as_mut() {
            egui::Panel::right("style_panel")
                .resizable(true)
                .default_size(320.0)
                .min_size(280.0)
                .show(ui, |ui| {
                    let a = review::style_panel(ui, r);
                    merge(a, &mut seek, &mut export, &mut rechunk_warn);
                });
        }
        if let Some(r) = self.review.as_mut() {
            egui::CentralPanel::default().show(ui, |ui| {
                let a = review::chunk_list(ui, r);
                merge(a, &mut seek, &mut export, &mut rechunk_warn);
            });
        }

        if rechunk_warn {
            self.warn = Some(
                "words-per-popup changed — transcript edits were discarded".into(),
            );
        }
        if export {
            self.start_export();
        } else if seek {
            // A re-chunk may have changed what (chunk, word) means; the caller
            // clears frame_key first when content moved under the same key.
            self.seek_preview();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.handle_drops(&ctx);

        egui::Panel::top("header").show(ui, |ui| {
            self.header(ui);
        });

        if let Some(err) = self.error.clone() {
            let color = ui.visuals().error_fg_color;
            if banner(ui, "error_banner", color, &err) {
                self.error = None;
            }
        }
        if let Some(warn) = self.warn.clone() {
            let color = ui.visuals().warn_fg_color;
            if banner(ui, "warn_banner", color, &warn) {
                self.warn = None;
            }
        }

        match self.stage {
            Stage::Drop => {
                egui::CentralPanel::default().show(ui, |ui| self.screen_drop(ui));
            }
            Stage::Ingest => {
                egui::CentralPanel::default().show(ui, |ui| self.screen_ingest(ui));
            }
            Stage::Review => self.review_stage(ui),
            Stage::Export => {
                egui::CentralPanel::default().show(ui, |ui| self.screen_export(ui));
            }
            Stage::Done => {
                egui::CentralPanel::default().show(ui, |ui| self.screen_done(ui));
            }
        }

        self.autosave_style(&ctx);
    }
}

/// Launch the GUI, optionally pre-loading a video.
pub fn run(initial: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("texxy {}", env!("TEXXY_GIT_HASH")))
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([1040.0, 660.0]),
        ..Default::default()
    };
    eframe::run_native(
        "texxy",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, initial)))),
    )
    .map_err(|e| anyhow::anyhow!("failed to start the GUI: {e}"))?;
    Ok(())
}

/// A dismissable warning/error strip pinned to the bottom of the window.
/// Returns `true` when the user clicked Dismiss.
fn banner(ui: &mut egui::Ui, id: &str, color: egui::Color32, text: &str) -> bool {
    let mut dismissed = false;
    egui::Panel::bottom(egui::Id::new(id)).show(ui, |ui| {
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
fn centered(ui: &mut egui::Ui, above_center: f32, add: impl FnOnce(&mut egui::Ui)) {
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

/// The shared ingest/export status block: title, progress (or spinner), label.
fn centered_block(ui: &mut egui::Ui, title: &str, label: &str, progress: Option<f32>) {
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
    });
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Open the folder containing `path` in the file manager.
fn reveal(path: &Path) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut cmd = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    });
    cmd.arg(dir).spawn()?;
    Ok(())
}
