//! The egui front-end: drop → ingest (progress) → review → export → done.
//!
//! All slow work (whisper, ffmpeg) runs on worker threads and reports through
//! a channel polled every UI frame; the UI thread never blocks.

mod licenses;
mod preview;
mod review;
mod theme;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{self, Style};
use crate::deps;
use crate::install::{self, InstallProgress};
use crate::model::{FoundModel, Model};
use crate::pipeline::{self, ExportProgress, ExportSink, IngestProgress, Progress};
use crate::source::Source;
use preview::Frame;
use review::{Action, ReviewState};

/// The project and developer, credited in the About window.
const DEVELOPER: &str = "Paul Buitenhuis";
const DEVELOPER_URL: &str = "https://github.com/PaulBuitenhuis";
const REPO_URL: &str = "https://github.com/PaulBuitenhuis/TextLayr";

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
    /// Whisper model in use, once known.
    model: Option<Model>,
}

struct ExportStatus {
    label: String,
    progress: f32,
}

struct DoneInfo {
    output: PathBuf,
    secs: f64,
}

/// Progress of a first-run install (ffmpeg/model downloads).
struct InstallStatus {
    label: String,
    /// `None` = indeterminate (verifying / starting).
    progress: Option<f32>,
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
    Install(InstallProgress),
    InstallOk,
    InstallErr(String),
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
    /// Preferred whisper model (persisted).
    model_pref: Model,
    /// A first-run install in progress, if any.
    install: Option<InstallStatus>,
    /// Cancels the in-flight install.
    install_cancel: Option<Arc<AtomicBool>>,
    /// The model the in-flight install is fetching (selected on success).
    install_target: Option<Model>,
    /// A model the user has asked to remove, awaiting confirmation.
    pending_remove: Option<Model>,
    /// Whether the About window is showing.
    about_open: bool,
    /// Whether the open-source licenses window is showing.
    licenses_open: bool,
    /// Selected section in the licenses window.
    licenses_sel: usize,
    /// The preferences currently persisted on disk; a change from the live
    /// style/model schedules an autosave.
    saved_prefs: config::Prefs,
    /// When the pending preference autosave may fire (debounce).
    prefs_save_at: Option<Instant>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Outstanding worker messages (drives the idle repoll).
    pending: usize,
    /// Generation counter so stale frame extractions are dropped.
    frame_token: u64,
    /// The wordmark banner shown in the header (None if it failed to decode).
    banner: Option<egui::TextureHandle>,
}

/// Decode the bundled wordmark banner into a texture for the header.
fn load_banner(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let bytes = include_bytes!("../../assets/banner-dark.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture("wordmark", color, egui::TextureOptions::LINEAR))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let saved_prefs = config::load_prefs();
        let deps = deps::Check::run(saved_prefs.model);
        // Adopt the model we actually resolved, so the choice shown to the user
        // is always the model in use (a stored preference that isn't installed
        // falls back to whichever model is). `autosave_prefs` persists the
        // change on the first frame.
        let model_pref = deps
            .model
            .as_ref()
            .map(|f| f.model)
            .unwrap_or(saved_prefs.model);
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
            model_pref,
            install: None,
            install_cancel: None,
            install_target: None,
            pending_remove: None,
            about_open: false,
            licenses_open: false,
            licenses_sel: 0,
            saved_prefs,
            prefs_save_at: None,
            tx,
            rx,
            pending: 0,
            frame_token: 0,
            banner: load_banner(&cc.egui_ctx),
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
    /// since launch) and clear the banner if everything is now present. If the
    /// preferred model is gone but another is installed, adopt that one so the
    /// shown choice stays equal to the model in use.
    fn recheck_deps(&mut self) {
        self.deps = deps::Check::run(self.model_pref);
        if let Some(found) = self.deps.model.as_ref() {
            if found.model != self.model_pref {
                self.model_pref = found.model;
                self.prefs_save_at = None;
            }
        }
        if self.deps.ok() {
            self.error = None;
        }
    }

    /// Delete a model TextLayr downloaded, then refresh discovery.
    fn remove_model(&mut self, found: &crate::model::FoundModel) {
        match crate::model::remove_downloaded(found) {
            Ok(()) => {
                self.pending_remove = None;
                self.recheck_deps();
            }
            Err(e) => {
                self.pending_remove = None;
                self.error = Some(format!("{e:#}"));
            }
        }
    }

    /// The style currently in effect: the review style, or the last saved one.
    fn current_style(&self) -> Style {
        self.review
            .as_ref()
            .map(|r| r.style.clone())
            .unwrap_or_else(|| self.saved_prefs.style.clone())
    }

    /// Persist preferences immediately — used when the model choice changes, so
    /// it sticks even if the app is quit before the debounce fires.
    fn persist_prefs(&mut self) {
        let prefs = config::Prefs {
            style: self.current_style(),
            model: self.model_pref,
        };
        self.saved_prefs = prefs.clone();
        self.prefs_save_at = None;
        if let Err(e) = config::save_prefs(&prefs) {
            eprintln!("      config save failed: {e:#}");
        }
    }

    /// Debounced autosave of the review style and the model preference. Called
    /// once per frame; writes the config only after things have been stable for
    /// a beat, so dragging a slider doesn't hammer the disk.
    fn autosave_prefs(&mut self, ctx: &egui::Context) {
        const DEBOUNCE: Duration = Duration::from_millis(400);
        let style_changed = match &self.review {
            Some(r) => r.style != self.saved_prefs.style,
            None => false,
        };
        let model_changed = self.model_pref != self.saved_prefs.model;
        if !style_changed && !model_changed {
            self.prefs_save_at = None;
            return;
        }
        let now = Instant::now();
        let deadline = *self.prefs_save_at.get_or_insert(now + DEBOUNCE);
        if now >= deadline {
            let prefs = config::Prefs {
                style: self.current_style(),
                model: self.model_pref,
            };
            self.saved_prefs = prefs.clone();
            self.prefs_save_at = None;
            if let Err(e) = config::save_prefs(&prefs) {
                eprintln!("      config save failed: {e:#}");
            }
        } else {
            // Make sure a frame runs at the deadline even if the user stops
            // interacting (egui otherwise sleeps).
            ctx.request_repaint_after(deadline - now);
        }
    }

    /// Download whatever is missing for `target` (ffmpeg and/or the model) and
    /// select that model when it succeeds.
    fn start_install(&mut self, target: Model) {
        if self.install.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.install_cancel = Some(Arc::clone(&cancel));
        self.install_target = Some(target);
        self.install = Some(InstallStatus {
            label: "starting…".into(),
            progress: None,
        });
        self.error = None;

        let tx = self.tx.clone();
        let done_tx = tx.clone();
        let sink: install::InstallSink = Box::new(move |p| {
            let _ = tx.send(Msg::Install(p));
        });
        self.pending += 1;
        std::thread::spawn(move || {
            let msg = match install::install(target, sink, cancel) {
                Ok(()) => Msg::InstallOk,
                Err(e) => Msg::InstallErr(format!("{e:#}")),
            };
            let _ = done_tx.send(msg);
        });
    }

    /// Ask the in-flight install to stop.
    fn cancel_install(&mut self) {
        if let Some(cancel) = &self.install_cancel {
            cancel.store(true, Ordering::Relaxed);
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
            model: None,
        });
        self.stage = Stage::Ingest;

        // The model the review will use: resolved during the dependency check.
        let Some(found_model) = self.deps.model.clone() else {
            self.ingest = None;
            self.stage = Stage::Drop;
            self.error = Some("no whisper model is installed".into());
            return;
        };

        let tx = self.tx.clone();
        let done_tx = tx.clone();
        let sink: Progress = Box::new(move |ev| {
            let _ = tx.send(Msg::Ingest(ev));
        });
        self.pending += 1;
        std::thread::spawn(move || {
            let msg = match pipeline::ingest(&path, false, &found_model, sink) {
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
                        IngestProgress::Model(m) => st.model = Some(m),
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
                            let model = st.model.unwrap_or_default();
                            st.label = format!("transcribing with whisper {}…", model.id());
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
                let style = self.saved_prefs.style.clone();
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
            Msg::Install(p) => {
                if let Some(st) = &mut self.install {
                    match p {
                        InstallProgress::NothingToDo => {
                            st.label = "nothing to install".into();
                            st.progress = Some(1.0);
                        }
                        InstallProgress::Begin { index, total, label } => {
                            st.label = format!("downloading {label} ({}/{total})", index + 1);
                            st.progress = Some(0.0);
                        }
                        InstallProgress::Bytes {
                            label,
                            got,
                            expected,
                            ..
                        } => {
                            st.label = match expected {
                                Some(t) if t > 0 => {
                                    format!("{label} — {} of {}", human_bytes(got), human_bytes(t))
                                }
                                _ => format!("{label} — {}", human_bytes(got)),
                            };
                            st.progress = expected
                                .filter(|t| *t > 0)
                                .map(|t| (got as f32 / t as f32).clamp(0.0, 1.0));
                        }
                        InstallProgress::Installed { index, total, label } => {
                            st.label = format!("installed {label} ({}/{total})", index + 1);
                            st.progress = Some(1.0);
                        }
                    }
                }
            }
            Msg::InstallOk => {
                self.pending = self.pending.saturating_sub(1);
                self.install = None;
                self.install_cancel = None;
                if let Some(target) = self.install_target.take() {
                    self.model_pref = target;
                }
                self.recheck_deps();
                if !self.deps.ok() {
                    self.error = self.deps.summary();
                }
            }
            Msg::InstallErr(e) => {
                self.pending = self.pending.saturating_sub(1);
                self.install = None;
                self.install_cancel = None;
                self.install_target = None;
                self.recheck_deps();
                // Cancellation is a user action, not something to shout about.
                if !e.contains("cancelled") {
                    eprintln!("      install failed: {e}");
                    self.error = Some(format!("install failed: {e}"));
                }
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
            match &self.banner {
                Some(banner) => {
                    // Render the wordmark at text-ish height, keeping its aspect.
                    let h = 24.0;
                    let w = h * banner.size()[0] as f32 / banner.size()[1] as f32;
                    ui.add(egui::Image::new(banner).fit_to_exact_size(egui::vec2(w, h)));
                }
                None => {
                    ui.label(egui::RichText::new("TextLayr").size(17.0).strong());
                }
            }
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
                if theme::button(ui, "About").clicked() {
                    self.about_open = true;
                }
            });
        });
        ui.add_space(1.0);
    }

    fn screen_drop(&mut self, ui: &mut egui::Ui) {
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
                egui::pos2(center.x, center.y + 104.0),
                egui::vec2(120.0, theme::CONTROL_H),
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
            if clicked {
                self.start_ingest(path);
            }
        }
    }

    /// Render the in-progress install, if any. Returns `true` when it drew
    /// something, so callers can skip the rest of the screen.
    fn screen_install(&mut self, ui: &mut egui::Ui) -> bool {
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
    fn screen_setup(&mut self, ui: &mut egui::Ui) {
        if self.screen_install(ui) {
            return;
        }
        ui.add_space(26.0);
        ui.vertical_centered(|ui| {
            ui.heading("Setup required");
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "TextLayr downloads ffmpeg and a whisper model the first time it runs.",
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
                ui.label(
                    egui::RichText::new("You can change this later from About.").weak(),
                );
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
                if theme::wide_button(ui, true, "New video").clicked() {
                    self.reset_to_drop();
                }
            });
        });
    }

    /// A small modal with the credits, version/build, the model selector and
    /// the resolved runtime paths (with their provenance).
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        let mut open_licenses = false;
        egui::Window::new("About TextLayr")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(470.0);
                ui.vertical_centered(|ui| {
                    ui.heading("TextLayr");
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new("Offline karaoke caption burner").weak());
                    ui.add_space(8.0);
                    ui.label(format!(
                        "Version {}   ·   build {}",
                        env!("CARGO_PKG_VERSION"),
                        env!("TEXTLAYR_GIT_HASH")
                    ));
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);

                // Credits + links.
                ui.vertical_centered(|ui| {
                    ui.label(format!("Made by {DEVELOPER}"));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        // Centre the row manually; egui has no "centered horizontal".
                        ui.add_space((ui.available_width() - 190.0).max(0.0) * 0.5);
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

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);

                // Models: what's installed, which is in use, and how to change
                // it. Only installed models can be selected; the rest offer a
                // Download button.
                let in_use = self.deps.model.as_ref().map(|f| f.model);
                let models: Vec<(Model, Option<FoundModel>)> = Model::ALL
                    .iter()
                    .map(|m| (*m, crate::model::locate(*m)))
                    .collect();
                let pending = self.pending_remove;
                let mut chosen = self.model_pref;
                let mut download: Option<Model> = None;
                let mut remove: Option<FoundModel> = None;
                let mut set_pending: Option<Option<Model>> = None;

                ui.label(egui::RichText::new("Model").strong());
                ui.add_space(4.0);
                for (m, found) in &models {
                    ui.horizontal(|ui| match found {
                        Some(f) => {
                            ui.radio_value(&mut chosen, *m, m.label());
                            let tip = if f.source == Source::Downloaded {
                                f.path.display().to_string()
                            } else {
                                format!(
                                    "{}\n\nTo remove it, delete that file — TextLayr \
                                     only deletes models it downloaded itself.",
                                    f.path.display()
                                )
                            };
                            if in_use == Some(*m) {
                                ui.label(
                                    egui::RichText::new("in use").color(theme::ACCENT).small(),
                                )
                                .on_hover_text(tip);
                            } else {
                                ui.label(egui::RichText::new(f.source.label()).weak().small())
                                    .on_hover_text(tip);
                            }
                            if f.source == Source::Downloaded {
                                if pending == Some(*m) {
                                    if theme::button(ui, "Confirm remove").clicked() {
                                        remove = Some(f.clone());
                                    }
                                    if theme::button(ui, "Keep").clicked() {
                                        set_pending = Some(None);
                                    }
                                } else if theme::button(ui, "Remove").clicked() {
                                    set_pending = Some(Some(*m));
                                }
                            }
                        }
                        None => {
                            ui.add_space(18.0);
                            ui.add(
                                egui::Label::new(egui::RichText::new(m.label()).weak()).truncate(),
                            );
                            if theme::button(ui, "Download").clicked() {
                                download = Some(*m);
                            }
                        }
                    });
                }
                // No model at all: make that explicit rather than looking fine.
                if models.iter().all(|(_, f)| f.is_none()) {
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("No model installed yet.")
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
                if let Some(m) = download {
                    // The download progress lives on the main screen, so get out
                    // of the way.
                    self.about_open = false;
                    self.start_install(m);
                } else if chosen != self.model_pref {
                    self.pending_remove = None;
                    self.model_pref = chosen;
                    self.persist_prefs();
                    self.recheck_deps();
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);

                let fftools = self.deps.fftools.as_ref();
                let ffmpeg = fftools.map(|t| (t.ffmpeg.clone(), t.source));
                let ffprobe = fftools.map(|t| (t.ffprobe.clone(), t.source));
                let model = self.deps.model.as_ref().map(|m| (m.path.clone(), m.source));
                let fonts = self.deps.fonts.clone();
                let config = config::config_path();

                egui::Grid::new("about_grid")
                    .num_columns(3)
                    .spacing([14.0, 6.0])
                    .min_col_width(62.0)
                    .max_col_width(320.0)
                    .show(ui, |ui| {
                        about_row(ui, "ffmpeg", ffmpeg.as_ref());
                        about_row(ui, "ffprobe", ffprobe.as_ref());
                        about_row(ui, "model", model.as_ref());
                        about_row(ui, "fonts", fonts.as_ref());
                        about_row_path(ui, "config", config.as_deref());
                    });

                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    if theme::fixed_button(ui, true, 120.0, "Close").clicked() {
                        close = true;
                    }
                });
            });
        if open_licenses {
            self.licenses_open = true;
        }
        if !open || close {
            self.about_open = false;
        }
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

        self.about_window(&ctx);
        licenses::window(&ctx, &mut self.licenses_open, &mut self.licenses_sel);
        self.autosave_prefs(&ctx);
    }
}

/// Launch the GUI, optionally pre-loading a video.
pub fn run(initial: Option<PathBuf>) -> Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("TextLayr")
        .with_inner_size([1320.0, 840.0])
        .with_min_inner_size([1040.0, 660.0]);
    // TextLayr's own icon. Without this, eframe falls back to the egui logo.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "textlayr",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, initial)))),
    )
    .map_err(|e| anyhow::anyhow!("failed to start the GUI: {e}"))?;
    Ok(())
}

/// One `label → path (provenance)` row in the About grid; a missing value shows
/// in red.
fn about_row(ui: &mut egui::Ui, label: &str, value: Option<&(PathBuf, Source)>) {
    ui.label(egui::RichText::new(label).weak());
    match value {
        Some((p, source)) => {
            ui.add(
                egui::Label::new(egui::RichText::new(p.display().to_string()).small().monospace())
                    .truncate(),
            )
            .on_hover_text(p.display().to_string());
            ui.label(egui::RichText::new(source.label()).weak().small());
        }
        None => {
            let color = ui.visuals().error_fg_color;
            ui.label(egui::RichText::new("not found").color(color));
            ui.label("");
        }
    }
    ui.end_row();
}

/// A `label → path` row with no provenance (used for the config path).
fn about_row_path(ui: &mut egui::Ui, label: &str, value: Option<&Path>) {
    ui.label(egui::RichText::new(label).weak());
    match value {
        Some(p) => {
            ui.add(
                egui::Label::new(egui::RichText::new(p.display().to_string()).small().monospace())
                    .truncate(),
            )
            .on_hover_text(p.display().to_string());
            ui.label("");
        }
        None => {
            let color = ui.visuals().error_fg_color;
            ui.label(egui::RichText::new("not found").color(color));
            ui.label("");
        }
    }
    ui.end_row();
}

/// Bytes as a short human string ("148 MB", "12.3 kB").
fn human_bytes(n: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The wordmark banner is embedded, decodes, and renders as an image
    /// without panicking (covers the `include_bytes!` asset + texture path).
    #[test]
    fn wordmark_banner_decodes_and_renders() {
        let ctx = egui::Context::default();
        let banner = load_banner(&ctx).expect("banner should decode");
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let h = 24.0;
            let w = h * banner.size()[0] as f32 / banner.size()[1] as f32;
            ui.add(egui::Image::new(&banner).fit_to_exact_size(egui::vec2(w, h)));
        });
        out.textures_delta.clear();
    }
}
