//! The egui front-end: drop → ingest (progress) → review → export → done.
//!
//! All slow work (whisper, ffmpeg) runs on worker threads and reports through
//! a channel polled every UI frame; the UI thread never blocks.

mod licenses;
mod preview;
mod profile;
mod review;
mod style;
mod theme;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{self, Grouping, Style};
use crate::deps;
use crate::install::{self, InstallProgress};
use crate::model::{FoundModel, Model};
use crate::pipeline::{self, ExportPhase, ExportProgress, ExportSink, IngestProgress, Progress};
use crate::profile::ProfileStore;
use crate::source::Source;
use crate::update;
use preview::Frame;
use profile::ProfileUi;
use review::{Action, ReviewState};

/// The project and developer, credited in the About window.
const DEVELOPER: &str = "Paul Buitenhuis";
const DEVELOPER_URL: &str = "https://github.com/PaulBuitenhuis";
const REPO_URL: &str = "https://github.com/PaulBuitenhuis/TextLayr";
/// Optional donations page, opened in the user's browser.
const SUPPORT_URL: &str = "https://ko-fi.com/U6R0290CU2";

/// Where the app is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Drop,
    Ingest,
    Review,
    Export,
    Done,
}

/// The selected tab in the About window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AboutTab {
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

struct IngestStatus {
    file: PathBuf,
    label: String,
    /// `None` = indeterminate (spinner) phase.
    progress: Option<f32>,
    /// Whisper model in use, once known.
    model: Option<Model>,
}

/// Live export progress: which pass is running and how far along it is.
struct ExportStatus {
    /// File being written, shown under the heading.
    file: String,
    /// The pass currently running.
    phase: ExportPhase,
    /// 0..=1 within the current pass (that pass's own bar).
    phase_frac: f32,
    /// 0..=1 across the whole export; drives the time estimate.
    overall: f32,
    /// Caption states the render pass will produce, for the step detail.
    states: usize,
    /// When the export started, for the time estimate.
    started: Instant,
}

impl ExportStatus {
    /// Estimated time left, derived from how far we are and how long it took.
    ///
    /// `None` burns the first couple of seconds (and the last sliver) rather
    /// than quoting a number that swings wildly.
    fn remaining(&self) -> Option<Duration> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let done = self.overall as f64;
        if elapsed < 2.0 || done <= 0.02 || done >= 1.0 {
            return None;
        }
        let left = (elapsed / done) - elapsed;
        // Round to 5s: the raw number jitters every frame and reads as noise.
        Some(Duration::from_secs_f64(
            ((left / 5.0).round() * 5.0).max(5.0),
        ))
    }
}

struct DoneInfo {
    output: PathBuf,
    secs: f64,
    width: u32,
    height: u32,
    duration_secs: f64,
}

/// Progress of a first-run install (ffmpeg/model downloads).
struct InstallStatus {
    label: String,
    /// `None` = indeterminate (verifying / starting).
    progress: Option<f32>,
}

/// Where the app-update check/install currently stands.
#[derive(Debug, Clone)]
enum UpdateUi {
    /// Not checked yet this session.
    Unknown,
    Checking,
    UpToDate,
    Available(update::Release),
    Installing {
        label: String,
        /// `None` = indeterminate (extracting / validating).
        progress: Option<f32>,
    },
    /// The bundle is about to be swapped; the app is closing to relaunch.
    Ready,
    /// Self-update wasn't possible; the installer was opened for a manual drag.
    OpenedDmg,
    Failed(String),
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
        width: u32,
        height: u32,
        duration_secs: f64,
    },
    ExportErr(String),
    Install(InstallProgress),
    InstallOk,
    InstallErr(String),
    /// Result of an update check; `manual` distinguishes a user-driven check
    /// (whose failures are shown) from the silent launch check.
    UpdateChecked {
        result: Result<update::Release, String>,
        manual: bool,
    },
    UpdateProgress(update::InstallProgress),
    UpdateInstalled(update::InstallOutcome),
    UpdateInstallErr(String),
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
    /// Cancels the in-flight export.
    export_cancel: Option<Arc<AtomicBool>>,
    /// The model the in-flight install is fetching (selected on success).
    install_target: Option<Model>,
    /// A model the user has asked to remove, awaiting confirmation.
    pending_remove: Option<Model>,
    /// Whether the Diagnostics tab is confirming removal of downloaded data.
    pending_remove_data: bool,
    /// App-update state (check / download / install).
    update: UpdateUi,
    /// Cancels the in-flight update check.
    update_check_cancel: Option<Arc<AtomicBool>>,
    /// Cancels the in-flight update download/install.
    update_install_cancel: Option<Arc<AtomicBool>>,
    /// Whether the About window is showing.
    about_open: bool,
    /// Selected tab in the About window.
    about_tab: AboutTab,
    /// Whether the open-source licenses window is showing.
    licenses_open: bool,
    /// Selected section in the licenses window.
    licenses_sel: usize,
    /// The preferences currently persisted on disk; a change from the live
    /// style/model schedules an autosave.
    saved_prefs: config::Prefs,
    /// When the pending preference autosave may fire (debounce).
    prefs_save_at: Option<Instant>,
    /// Built-in and saved style profiles.
    profiles: ProfileStore,
    /// Profile dropdown/management state (active id, dirty flag).
    profile_ui: ProfileUi,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Outstanding worker messages (drives the idle repoll).
    pending: usize,
    /// Generation counter so stale frame extractions are dropped.
    frame_token: u64,
    /// The wordmark banner, shown in the header and the About window
    /// (None if it failed to decode).
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
        let profiles = ProfileStore::new();
        let profile_ui = ProfileUi::new(saved_prefs.active_profile.clone(), &profiles);
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
            export_cancel: None,
            install_target: None,
            pending_remove: None,
            pending_remove_data: false,
            update: UpdateUi::Unknown,
            update_check_cancel: None,
            update_install_cancel: None,
            about_open: false,
            about_tab: AboutTab::About,
            licenses_open: false,
            licenses_sel: 0,
            saved_prefs,
            prefs_save_at: None,
            profiles,
            profile_ui,
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

    /// Delete a model file TextLayr manages, then refresh discovery.
    fn remove_model(&mut self, found: &crate::model::FoundModel) {
        match crate::model::remove_located(found) {
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

    /// A plain-text snapshot of the resolved runtime pieces, for pasting into a
    /// bug report from the Diagnostics tab.
    fn copy_diagnostics_report(&self) -> String {
        fn line(out: &mut String, label: &str, value: Option<&(PathBuf, Source)>) {
            match value {
                Some((p, source)) => {
                    out.push_str(&format!("{label}: {} ({})\n", p.display(), source.label()));
                }
                None => out.push_str(&format!("{label}: not found\n")),
            }
        }

        let mut out = String::new();
        out.push_str(&format!(
            "TextLayr {} (build {})\n",
            env!("CARGO_PKG_VERSION"),
            env!("TEXTLAYR_GIT_HASH")
        ));
        out.push_str(&format!(
            "OS: {} / {}\n\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));

        let fftools = self.deps.fftools.as_ref();
        let ffmpeg = fftools.map(|t| (t.ffmpeg.clone(), t.source));
        let ffprobe = fftools.map(|t| (t.ffprobe.clone(), t.source));
        let model = self.deps.model.as_ref().map(|m| (m.path.clone(), m.source));
        line(&mut out, "ffmpeg", ffmpeg.as_ref());
        line(&mut out, "ffprobe", ffprobe.as_ref());
        line(&mut out, "model", model.as_ref());
        line(&mut out, "fonts", self.deps.fonts.as_ref());
        match config::config_path() {
            Some(p) => out.push_str(&format!("config: {}\n", p.display())),
            None => out.push_str("config: (default location)\n"),
        }
        out
    }

    /// The style currently in effect: the review style, or the last saved one.
    fn current_style(&self) -> Style {
        self.review
            .as_ref()
            .map(|r| r.style.clone())
            .unwrap_or_else(|| self.saved_prefs.style.clone())
    }

    /// The grouping currently in effect: the review state, or the last saved
    /// one before a video is loaded.
    fn current_grouping(&self) -> Grouping {
        self.review
            .as_ref()
            .map(|r| r.grouping.clone())
            .unwrap_or_else(|| self.saved_prefs.grouping.clone())
    }

    /// The complete preferences to persist: live style/grouping/model plus the
    /// update settings (which are changed directly on `saved_prefs`).
    fn prefs_now(&self) -> config::Prefs {
        config::Prefs {
            style: self.current_style(),
            grouping: self.current_grouping(),
            active_profile: self.profile_ui.active_id.clone(),
            model: self.model_pref,
            auto_check_updates: self.saved_prefs.auto_check_updates,
            last_update_check: self.saved_prefs.last_update_check,
        }
    }

    /// Persist preferences immediately — used when the model choice changes, so
    /// it sticks even if the app is quit before the debounce fires.
    fn persist_prefs(&mut self) {
        let prefs = self.prefs_now();
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
        let (style_changed, grouping_changed) = match &self.review {
            Some(r) => (
                r.style != self.saved_prefs.style,
                r.grouping != self.saved_prefs.grouping,
            ),
            None => (false, false),
        };
        let model_changed = self.model_pref != self.saved_prefs.model;
        let profile_changed = self.profile_ui.active_id != self.saved_prefs.active_profile;
        if !style_changed && !grouping_changed && !model_changed && !profile_changed {
            self.prefs_save_at = None;
            return;
        }
        let now = Instant::now();
        let deadline = *self.prefs_save_at.get_or_insert(now + DEBOUNCE);
        if now >= deadline {
            let prefs = self.prefs_now();
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

    // ── updates ───────────────────────────────────────────────────────────

    /// Auto-check on launch when enabled and not checked in the last day.
    fn maybe_auto_check_updates(&mut self) {
        if !self.saved_prefs.auto_check_updates {
            return;
        }
        if !matches!(self.update, UpdateUi::Unknown) {
            return;
        }
        if let Some(last) = self.saved_prefs.last_update_check {
            const A_DAY: u64 = 24 * 60 * 60;
            if now_unix().saturating_sub(last) < A_DAY {
                return;
            }
        }
        self.start_update_check(false);
    }

    /// Ask GitHub for the newest release. `manual` checks surface failures and
    /// ignore the throttle.
    fn start_update_check(&mut self, manual: bool) {
        if matches!(
            self.update,
            UpdateUi::Checking | UpdateUi::Installing { .. }
        ) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.update_check_cancel = Some(Arc::clone(&cancel));
        self.update = UpdateUi::Checking;

        let tx = self.tx.clone();
        self.pending += 1;
        std::thread::spawn(move || {
            let result = update::fetch_latest(&cancel).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::UpdateChecked { result, manual });
        });
    }

    /// Download and install `release` on a worker thread.
    fn start_update_install(&mut self, release: update::Release) {
        if matches!(self.update, UpdateUi::Installing { .. }) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.update_install_cancel = Some(Arc::clone(&cancel));
        self.update = UpdateUi::Installing {
            label: "starting…".into(),
            progress: None,
        };
        self.error = None;

        let tx = self.tx.clone();
        let done_tx = tx.clone();
        self.pending += 1;
        std::thread::spawn(move || {
            let mut sink: update::InstallSink = Box::new(move |p| {
                let _ = tx.send(Msg::UpdateProgress(p));
            });
            let msg = match update::install(&release, &cancel, &mut sink) {
                Ok(o) => Msg::UpdateInstalled(o),
                Err(e) => Msg::UpdateInstallErr(format!("{e:#}")),
            };
            let _ = done_tx.send(msg);
        });
    }

    /// Ask the in-flight update download/install to stop.
    fn cancel_update_install(&mut self) {
        if let Some(cancel) = &self.update_install_cancel {
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
        let Some(r) = self.review.as_mut() else {
            return;
        };
        r.preview.invalidate();
        let key = (r.selected, r.active_word);
        if r.frame_key == key {
            return;
        }
        r.frame_key = key;
        let at = r.preview_time();
        let tools = r.tools.clone();
        let input = r.input.clone();

        // Bumped, not assigned: `reset_to_drop` parks this counter so that
        // in-flight extractions fall on the floor, and a plain `+= 1` used to
        // overflow the moment a second video was opened after an export.
        // Bumped, not assigned: `reset_to_drop` parks this counter so that
        // in-flight extractions fall on the floor, and a plain `+= 1` used to
        // overflow the moment a second video was opened after an export.
        self.frame_token = self.frame_token.wrapping_add(1);
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
        // The render pass makes one PNG per caption state; the step row shows
        // the count, and it is the same arithmetic the pipeline does.
        let states: usize = chunks.iter().map(|c| c.state_windows().len()).sum();

        self.error = None;
        self.export = Some(ExportStatus {
            file: file_label(&output),
            phase: ExportPhase::Rendering,
            phase_frac: 0.0,
            overall: 0.0,
            states,
            started: Instant::now(),
        });
        self.stage = Stage::Export;

        let cancel = Arc::new(AtomicBool::new(false));
        self.export_cancel = Some(Arc::clone(&cancel));

        let tx = self.tx.clone();
        let done_tx = tx.clone();
        let sink: ExportSink = Box::new(move |p| {
            let _ = tx.send(Msg::Export(p));
        });
        let dims = (info.display_width, info.display_height, info.duration_secs);
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
                &cancel,
                sink,
            ) {
                Ok(()) => {
                    let (width, height, duration_secs) = dims;
                    let _ = done_tx.send(Msg::ExportOk {
                        output,
                        secs: started.elapsed().as_secs_f64(),
                        width,
                        height,
                        duration_secs,
                    });
                }
                Err(e) => {
                    let _ = done_tx.send(Msg::ExportErr(format!("{e:#}")));
                }
            }
        });
    }

    /// Ask the in-flight export to stop; the worker kills ffmpeg and drops the
    /// half-written file, then reports a cancel back through the message pump.
    fn cancel_export(&mut self) {
        if let Some(cancel) = &self.export_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn reset_to_drop(&mut self) {
        // If an export is somehow still in flight, stop it rather than leaving
        // a worker holding a temp dir and a half-written file.
        self.cancel_export();
        self.export_cancel = None;
        self.stage = Stage::Drop;
        self.input = None;
        self.ingest = None;
        self.review = None;
        self.export = None;
        self.done = None;
        self.error = None;
        self.warn = None;
        // Invalidate any in-flight frame extraction: its token no longer
        // matches, so the result is dropped when it arrives.
        self.frame_token = self.frame_token.wrapping_add(1);
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
                let style = self.current_style();
                let grouping = self.current_grouping();
                self.review = Some(ReviewState::new(input, *ing, style, grouping));
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
                    st.phase = p.phase;
                    st.phase_frac = p.phase_frac;
                    st.overall = p.overall;
                }
            }
            Msg::ExportOk {
                output,
                secs,
                width,
                height,
                duration_secs,
            } => {
                self.pending = self.pending.saturating_sub(1);
                self.export = None;
                self.export_cancel = None;
                self.done = Some(DoneInfo {
                    output,
                    secs,
                    width,
                    height,
                    duration_secs,
                });
                self.stage = Stage::Done;
            }
            Msg::ExportErr(e) => {
                self.pending = self.pending.saturating_sub(1);
                self.export = None;
                self.export_cancel = None;
                // Cancelling is the user's own doing: drop back to review, with
                // every edit intact, and don't shout about it.
                if e.contains("cancelled") {
                    self.stage = Stage::Review;
                } else {
                    // The banner shows this to the user; stderr is the only
                    // place a headless harness (or a terminal user) can see it.
                    eprintln!("      export failed: {e}");
                    self.error = Some(e);
                    self.stage = Stage::Review;
                }
            }
            Msg::Install(p) => {
                if let Some(st) = &mut self.install {
                    match p {
                        InstallProgress::NothingToDo => {
                            st.label = "nothing to install".into();
                            st.progress = Some(1.0);
                        }
                        InstallProgress::Begin {
                            index,
                            total,
                            label,
                        } => {
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
                        InstallProgress::Installed {
                            index,
                            total,
                            label,
                        } => {
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
            Msg::UpdateChecked { result, manual } => {
                self.pending = self.pending.saturating_sub(1);
                self.update_check_cancel = None;
                // Record the attempt either way so the daily throttle holds
                // even across a failed (e.g. offline) check.
                self.saved_prefs.last_update_check = Some(now_unix());
                self.persist_prefs();
                match result {
                    Ok(release) => {
                        self.update = if update::forced()
                            || update::is_newer(update::current_version(), &release.version)
                        {
                            UpdateUi::Available(release)
                        } else {
                            UpdateUi::UpToDate
                        };
                    }
                    Err(e) => {
                        self.update = if manual {
                            UpdateUi::Failed(e)
                        } else {
                            // A silent check that can't reach the network just
                            // leaves the app looking unchecked.
                            UpdateUi::Unknown
                        };
                    }
                }
            }
            Msg::UpdateProgress(p) => {
                if let UpdateUi::Installing { label, progress } = &mut self.update {
                    match p {
                        update::InstallProgress::Downloading { got, total } => {
                            *label = match total {
                                Some(t) if t > 0 => format!(
                                    "downloading TextLayr — {} of {}",
                                    human_bytes(got),
                                    human_bytes(t)
                                ),
                                _ => format!("downloading TextLayr — {}", human_bytes(got)),
                            };
                            *progress = total
                                .filter(|t| *t > 0)
                                .map(|t| (got as f32 / t as f32).clamp(0.0, 1.0));
                        }
                        update::InstallProgress::Extracting => {
                            *label = "extracting…".into();
                            *progress = None;
                        }
                        update::InstallProgress::Validating => {
                            *label = "checking the download…".into();
                            *progress = None;
                        }
                        update::InstallProgress::Launching => {
                            *label = "finishing up…".into();
                            *progress = None;
                        }
                        update::InstallProgress::OpeningDmg => {
                            *label = "opening the installer…".into();
                            *progress = None;
                        }
                    }
                }
            }
            Msg::UpdateInstalled(outcome) => {
                self.pending = self.pending.saturating_sub(1);
                self.update_install_cancel = None;
                self.update = match outcome {
                    update::InstallOutcome::SelfUpdate => UpdateUi::Ready,
                    update::InstallOutcome::OpenedDmg => UpdateUi::OpenedDmg,
                };
            }
            Msg::UpdateInstallErr(e) => {
                self.pending = self.pending.saturating_sub(1);
                self.update_install_cancel = None;
                self.update = UpdateUi::Failed(format!("update failed: {e}"));
                // Cancellation is a user action, not an error to shout about.
                if !e.contains("cancelled") {
                    eprintln!("      update failed: {e}");
                    self.error = Some(format!("update failed: {e}"));
                }
            }
        }
    }

    // ── input ─────────────────────────────────────────────────────────────

    fn handle_drops(&mut self, ctx: &egui::Context) {
        // egui 0.36: dropped files are backend handles behind the `DroppedFile`
        // trait on `raw`; paths are absolute on native platforms.
        let (dropped, hovered) = ctx.input(|i| {
            let paths: Vec<PathBuf> = i
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect();
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
        // Fill the bar's content box and centre the row in it, so the padding
        // above and below is equal whatever the widgets happen to measure.
        let h = ui.available_height();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
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
                    if theme::button(ui, "Settings").clicked() {
                        self.about_open = true;
                    }
                    if matches!(&self.update, UpdateUi::Available(_))
                        && theme::button(ui, "Update available").clicked()
                    {
                        self.about_open = true;
                    }
                });
            },
        );
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

    fn screen_ingest(&mut self, ui: &mut egui::Ui) {
        let Some(st) = &self.ingest else { return };
        let file = file_label(&st.file);
        let label = st.label.clone();
        centered_block(ui, &file, &label, st.progress);
    }

    /// The export in progress: one row per pass, each with its own bar, plus a
    /// time estimate and a way out.
    fn screen_export(&mut self, ui: &mut egui::Ui) {
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

    fn screen_done(&mut self, ui: &mut egui::Ui) {
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
            ui.label(egui::RichText::new(file_label(&output)).strong());
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

    fn about_window(&mut self, ctx: &egui::Context) {
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
                ui.set_width(476.0);

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
                                    ui.label(
                                        egui::RichText::new("Offline karaoke caption burner")
                                            .weak(),
                                    );
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
                                            "TextLayr is free and always will be — a \
                                             one-person project. If it saved you time, \
                                             a coffee is always appreciated.",
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
                                            if theme::button(ui, "Download & install").clicked() {
                                                start_install_update = Some(r.clone());
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
                                        ui.label(
                                            egui::RichText::new(
                                                "The installer is open — drag TextLayr into \
                                                 Applications, then quit and reopen it.",
                                            )
                                            .weak(),
                                        );
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

    // ── review layout ─────────────────────────────────────────────────────

    fn review_stage(&mut self, ui: &mut egui::Ui) {
        let mut seek = false;
        let mut export = false;
        let mut rechunk_warn = false;

        let merge = |a: Action, seek: &mut bool, export: &mut bool, rechunk: &mut bool| match a {
            Action::None => {}
            Action::Seek => *seek = true,
            Action::Export => *export = true,
            Action::Rechunked { discarded_edits } => {
                *seek = true;
                *rechunk = *rechunk || discarded_edits;
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
                .default_size(340.0)
                .min_size(300.0)
                .show(ui, |ui| {
                    let a = review::style_panel(ui, r, &self.profiles, &mut self.profile_ui);
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
            self.warn = Some("words-per-popup changed — transcript edits were discarded".into());
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
        self.maybe_auto_check_updates();

        // ⌘, is the standard "open settings" chord on macOS.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
            self.about_open = true;
        }

        egui::Panel::top("header")
            .exact_size(theme::HEADER_H)
            .show(ui, |ui| {
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

        // The self-update helper is waiting for this process to exit before it
        // swaps the bundle, so close the window once it is armed.
        if matches!(self.update, UpdateUi::Ready) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
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

/// One entry in the Diagnostics tab: a status dot, the item name and its
/// provenance, the full (wrapped, selectable) path, and copy/reveal actions.
///
/// Returns a path to reveal in the file manager if the user asked for it.
fn diagnostic_entry(
    ui: &mut egui::Ui,
    label: &str,
    value: Option<(&Path, Option<&str>)>,
) -> Option<PathBuf> {
    let mut reveal = None;
    ui.horizontal_wrapped(|ui| {
        status_dot(ui, value.is_some());
        ui.label(egui::RichText::new(label).strong());
        match value {
            Some((_, Some(source))) => {
                ui.label(egui::RichText::new(source).weak().small());
            }
            Some((_, None)) => {}
            None => {
                ui.label(
                    egui::RichText::new("not found")
                        .color(ui.visuals().error_fg_color)
                        .small(),
                );
            }
        }
    });
    if let Some((path, _)) = value {
        let text = path.display().to_string();
        ui.add(
            egui::Label::new(egui::RichText::new(text.clone()).monospace().small())
                .wrap()
                .selectable(true),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::button(ui, "Copy").clicked() {
                ui.ctx().copy_text(text.clone());
            }
            if theme::button(ui, reveal_label()).clicked() {
                reveal = Some(path.to_path_buf());
            }
        });
    }
    ui.add_space(10.0);
    reveal
}

/// "Reveal in Finder" on macOS, "Show in folder" elsewhere.
fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in folder"
    }
}

/// A small filled dot used for the Diagnostics status (drawn, not a glyph —
/// the bundled UI fonts have no suitable circle character).
fn status_dot(ui: &mut egui::Ui, ok: bool) {
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
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// Width of the centred column the export and done screens lay out in: wide
/// enough for the metadata line, narrow enough not to sprawl on a big window.
const COLUMN_W: f32 = 460.0;

/// Run `add` inside a fixed-width column, centred in the space left.
///
/// Two egui details make this more than a `set_max_width` call: full-width
/// `add_space` calls widen the ui's `min_rect` (and egui never shrinks below
/// that), and a panel's own layout is `Align::Min`, so a child is placed at the
/// left however narrow it is. An explicit centred rect avoids both — and gives
/// inner `right_to_left` layouts the column's right edge to align to.
fn column<R>(ui: &mut egui::Ui, height: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
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
fn centred_buttons(ui: &mut egui::Ui, labels: &[&str]) -> Vec<egui::Response> {
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

/// How one export pass reads in the step list.
#[derive(Clone, Copy)]
enum Pass {
    /// Not started.
    Waiting,
    /// Running now, with its own 0..=1 fraction.
    Running(f32),
    /// Finished (so the list length stays stable while the export runs).
    Done,
}

/// One row of the export step list: a status mark, the pass name, and either its
/// own bar (while running), a detail (once done), or nothing (still to come).
fn export_pass(ui: &mut egui::Ui, phase: ExportPhase, state: Pass, detail: &str) {
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
fn mmss(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// A loose duration for elapsed/estimated time ("48s", "2m 12s"). Deliberately
/// not a clock time, so it can't be mistaken for a deadline.
fn rough_secs(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 60.0 {
        format!("{s:.0}s")
    } else if s < 3600.0 {
        format!("{}m {:02}s", (s / 60.0) as u64, (s as u64) % 60)
    } else {
        format!("{}h {:02}m", (s / 3600.0) as u64, ((s as u64) % 3600) / 60)
    }
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

/// Open `path` in the OS default application (e.g. the video's player).
fn open_file(path: &Path) -> Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg("--");
        c
    };
    cmd.arg(path);
    let mut child = cmd.spawn()?;
    // Reap on a detached thread: the launcher exits immediately, and leaving it
    // unreaped holds a zombie for the rest of the session.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Open the folder containing `path` in the file manager.
fn reveal(path: &Path) -> Result<()> {
    crate::platform::reveal_in_file_manager(path)
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

    /// A full-width control inside [`column`] fills the column, not the window —
    /// the bug that made the export-complete screen three edge-to-edge slabs —
    /// and the column itself is centred rather than hugging the left edge.
    #[test]
    fn a_full_width_control_is_bounded_and_centred_by_the_column() {
        egui::__run_test_ui(|ui| {
            let avail = ui.available_rect_before_wrap();
            let mut button = egui::Rect::ZERO;
            column(ui, 300.0, |ui| {
                button = theme::primary_button(ui, true, "Open video").rect;
            });
            assert!(
                (button.width() - COLUMN_W).abs() < 1.0,
                "button should be {COLUMN_W} wide, got {}",
                button.width()
            );
            assert!(
                (button.center().x - avail.center().x).abs() < 1.0,
                "column should be centred: {:?} vs {:?}",
                button.center().x,
                avail.center().x
            );
        });
    }

    /// The two secondary actions are centred as a group even though their labels
    /// differ in length (which is what made them look off-centre before).
    #[test]
    fn the_action_pair_is_centred_as_a_group() {
        let ctx = egui::Context::default();
        let mut seen = None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let avail = ui.available_rect_before_wrap();
            column(ui, 200.0, |ui| {
                let rs = centred_buttons(ui, &[reveal_label(), "New video"]);
                seen = Some((rs[0].rect, rs[1].rect, avail.center().x));
            });
        });
        out.textures_delta.clear();
        let (a, b, cx) = seen.expect("buttons drawn");
        assert!(
            a.width() != b.width(),
            "labels differ, so widths should too"
        );
        let group = (a.left() + b.right()) / 2.0;
        assert!(
            (group - cx).abs() < 1.0,
            "group centred at {group}, want {cx}"
        );
    }

    /// Diagnostics entries render for both a resolved and a missing item
    /// without panicking, and the reveal label matches the platform.
    #[test]
    fn diagnostics_entries_render() {
        egui::__run_test_ui(|ui| {
            let found = PathBuf::from("/usr/local/bin/ffmpeg");
            let present: Option<(&Path, Option<&str>)> =
                Some((found.as_path(), Some("system (PATH)")));
            assert!(diagnostic_entry(ui, "ffmpeg", present).is_none());
            let absent: Option<(&Path, Option<&str>)> = None;
            assert!(diagnostic_entry(ui, "model", absent).is_none());
        });
        assert_eq!(reveal_label().contains("Finder"), cfg!(target_os = "macos"));
    }
}
