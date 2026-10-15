//! The egui front-end: drop → ingest (progress) → review → export → done.
//!
//! All slow work (whisper, ffmpeg) runs on worker threads and reports through
//! a channel polled every UI frame; the UI thread never blocks.

mod preview;
mod review;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::Result;

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
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Outstanding worker messages (drives the idle repoll).
    pending: usize,
    /// Generation counter so stale frame extractions are dropped.
    frame_token: u64,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = mpsc::channel();
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
            tx,
            rx,
            pending: 0,
            frame_token: 0,
        };
        if let Some(path) = initial {
            if path.is_file() {
                app.start_ingest(path);
            } else {
                app.error = Some(format!("input file not found: {}", path.display()));
            }
        }
        app
    }

    // ── workers ───────────────────────────────────────────────────────────

    fn start_ingest(&mut self, path: PathBuf) {
        self.error = None;
        self.warn = None;
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
                self.review = Some(ReviewState::new(input, *ing));
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
            ui.label(egui::RichText::new("texxy").size(18.0).strong());
            ui.separator();
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
                if self.stage == Stage::Review && ui.button("Change video").clicked() {
                    self.reset_to_drop();
                }
            });
        });
    }

    fn screen_drop(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let (fill, stroke) = if self.hovering {
            (
                ui.visuals().selection.bg_fill,
                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
            )
        } else {
            (
                ui.visuals().faint_bg_color,
                egui::Stroke::new(
                    1.5,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                ),
            )
        };
        ui.painter().rect_filled(rect, 16.0, fill);
        ui.painter().rect_stroke(rect, 16.0, stroke, egui::StrokeKind::Inside);
        ui.painter().text(
            rect.center() - egui::vec2(0.0, 24.0),
            egui::Align2::CENTER_CENTER,
            "Drop a video here",
            egui::FontId::proportional(32.0),
            ui.visuals().strong_text_color(),
        );
        ui.painter().text(
            rect.center() + egui::vec2(0.0, 20.0),
            egui::Align2::CENTER_CENTER,
            "any video with a voiceover track\nor run:  texxy <video.mp4>",
            egui::FontId::proportional(15.0),
            ui.visuals().weak_text_color(),
        );

        // A startup file that failed ingest: offer a retry.
        if let Some(path) = self.input.clone() {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("failed: {}", file_label(&path))).weak());
                if ui.button("Retry").clicked() {
                    self.start_ingest(path);
                }
            });
        }
    }

    fn screen_ingest(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.ingest else { return };
        let file = file_label(&st.file);
        let label = st.label.clone();
        let progress = st.progress;
        ui.vertical_centered(|ui| {
            ui.add_space(70.0);
            ui.heading(file);
            ui.add_space(20.0);
            match progress {
                Some(p) => {
                    ui.add(
                        egui::ProgressBar::new(p)
                            .show_percentage()
                            .desired_width(420.0),
                    );
                }
                None => {
                    ui.add(egui::Spinner::new());
                }
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new(label).weak());
        });
    }

    fn screen_export(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.export else { return };
        let label = st.label.clone();
        let progress = st.progress;
        ui.vertical_centered(|ui| {
            ui.add_space(70.0);
            ui.heading("Exporting");
            ui.add_space(20.0);
            ui.add(
                egui::ProgressBar::new(progress)
                    .show_percentage()
                    .desired_width(420.0),
            );
            ui.add_space(10.0);
            ui.label(egui::RichText::new(label).weak());
        });
    }

    fn screen_done(&mut self, ui: &mut egui::Ui) {
        let Some(d) = &self.done else { return };
        let output = d.output.clone();
        let secs = d.secs;
        ui.vertical_centered(|ui| {
            ui.add_space(70.0);
            ui.heading("Done");
            ui.add_space(12.0);
            let size_mb = std::fs::metadata(&output).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
            ui.label(
                egui::RichText::new(format!("{}  ({size_mb:.1} MB, export took {secs:.0}s)", output.display()))
                    .monospace(),
            );
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if ui.button("Reveal in folder").clicked() {
                    if let Err(e) = reveal(&output) {
                        self.error = Some(format!("could not open folder: {e}"));
                    }
                }
                if ui.button("New video").clicked() {
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
                .default_size(460.0)
                .min_size(300.0)
                .show(ui, |ui| {
                    let a = review::preview_panel(ui, r);
                    merge(a, &mut seek, &mut export, &mut rechunk_warn);
                });
        }
        if let Some(r) = self.review.as_mut() {
            egui::Panel::right("style_panel")
                .resizable(true)
                .default_size(300.0)
                .min_size(250.0)
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
            let mut dismiss = false;
            egui::Panel::bottom("error_banner").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, format!("⚠ {err}"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").clicked() {
                            dismiss = true;
                        }
                    });
                });
            });
            if dismiss {
                self.error = None;
            }
        }
        if let Some(warn) = self.warn.clone() {
            let mut dismiss = false;
            egui::Panel::bottom("warn_banner").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(255, 170, 0), format!("⚠ {warn}"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").clicked() {
                            dismiss = true;
                        }
                    });
                });
            });
            if dismiss {
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
    }
}

/// Launch the GUI, optionally pre-loading a video.
pub fn run(initial: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("texxy")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([960.0, 620.0]),
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
