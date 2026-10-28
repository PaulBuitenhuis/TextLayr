//! The egui front-end: drop → ingest (progress) → review → export → done.
//!
//! All slow work (whisper, ffmpeg) runs on worker threads and reports through
//! a channel polled every UI frame; the UI thread never blocks.

mod licenses;
mod preview;
mod profile;
mod review;
mod screens;
mod settings;
mod style;
mod theme;
mod widgets;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{self, Grouping, Style};
use crate::deps;
use crate::install::{self, InstallProgress};
use crate::model::Model;
use crate::output;
use crate::pipeline::{self, ExportPhase, ExportProgress, ExportSink, IngestProgress, Progress};
use crate::probe::Fftools;
use crate::profile::ProfileStore;
use crate::source::Source;
use crate::update;
use profile::ProfileUi;
use review::{Action, ReviewState};
use settings::AboutTab;
use widgets::{banner, file_label, human_bytes, now_unix, reveal_label, status_dot};

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

/// Everything an export needs, captured before the save dialog opens.
///
/// The dialog blocks the UI thread, so the review state cannot change while it
/// is open; cloning here keeps the worker independent of `self`.
struct ExportPlan {
    input: PathBuf,
    tools: crate::probe::Fftools,
    info: crate::probe::VideoInfo,
    chunks: Vec<crate::chunk::Chunk>,
    style: Style,
    states: usize,
}

/// A native picker to run after the current frame.
///
/// egui must not re-enter `self` (or run a nested modal loop) from inside a
/// draw closure, so panels only record the request and [`App::service_pick`]
/// opens the dialog once the frame is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickRequest {
    OpenVideo,
    SaveExport,
}

/// One background preview-frame request. `prefetch` requests are dropped in
/// favour of a newer current request by the worker.
#[derive(Clone)]
struct PreviewJob {
    generation: u64,
    chunk: usize,
    key: preview::FrameKey,
    at: f64,
    src_width: u32,
    src_height: u32,
    tools: Fftools,
    input: PathBuf,
    prefetch: bool,
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
    PreviewFrame {
        generation: u64,
        key: preview::FrameKey,
        frame: Option<preview::Frame>,
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
    /// Sends preview-frame requests to the single extraction worker.
    preview_tx: Sender<PreviewJob>,
    /// Bumped when the video changes, so old frames are discarded.
    preview_generation: u64,
    /// Recently extracted popup frames (bounded LRU).
    frame_cache: preview::FrameCache,
    /// The popup key whose frame is currently shown.
    applied_frame: Option<preview::FrameKey>,
    /// The popup key currently being extracted, if any.
    requested_frame: Option<preview::FrameKey>,
    /// A native picker the current frame asked for.
    pending_pick: Option<PickRequest>,
    /// A destination that already exists and needs an in-app confirmation
    /// (always on Linux; on macOS when we changed the panel's chosen name).
    pending_overwrite: Option<(ExportPlan, output::ExportTarget)>,
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

/// One preview extraction worker.
///
/// Current requests take priority and coalesce to the latest; prefetches wait
/// in a tiny queue. One ffmpeg process at a time keeps the UI and the export
/// path responsive, and rapid clicks cannot fan out into a pile of decoders.
fn preview_worker(rx: Receiver<PreviewJob>, messages: Sender<Msg>) {
    let mut current: Option<PreviewJob> = None;
    let mut prefetch: std::collections::VecDeque<PreviewJob> = std::collections::VecDeque::new();

    loop {
        if current.is_none() && prefetch.is_empty() {
            match rx.recv() {
                Ok(job) => {
                    if job.prefetch {
                        prefetch.push_back(job);
                    } else {
                        current = Some(job);
                    }
                }
                Err(_) => return,
            }
        }
        while let Ok(job) = rx.try_recv() {
            if job.prefetch {
                if !prefetch
                    .iter()
                    .any(|queued| queued.key == job.key && queued.generation == job.generation)
                {
                    prefetch.push_back(job);
                    while prefetch.len() > 4 {
                        prefetch.pop_front();
                    }
                }
            } else {
                current = Some(job);
            }
        }
        let Some(job) = current.take().or_else(|| prefetch.pop_front()) else {
            continue;
        };
        let frame = match preview::extract(
            &job.tools,
            &job.input,
            job.at,
            job.src_width,
            job.src_height,
        ) {
            Ok(frame) => Some(frame),
            Err(e) => {
                eprintln!("      preview frame: {e:#}");
                None
            }
        };
        let _ = messages.send(Msg::PreviewFrame {
            generation: job.generation,
            key: job.key,
            frame,
        });
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let (preview_tx, preview_rx) = mpsc::channel::<PreviewJob>();
        let preview_messages = tx.clone();
        std::thread::spawn(move || preview_worker(preview_rx, preview_messages));
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
            preview_tx,
            preview_generation: 0,
            frame_cache: preview::FrameCache::new(8),
            applied_frame: None,
            requested_frame: None,
            pending_pick: None,
            pending_overwrite: None,
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
            last_export_dir: self.saved_prefs.last_export_dir.clone(),
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

    /// Build the extraction request for the selected popup.
    fn preview_target(&self) -> Option<PreviewJob> {
        let r = self.review.as_ref()?;
        let key = r.chunk_key(r.selected)?;
        Some(PreviewJob {
            generation: self.preview_generation,
            chunk: r.selected,
            key,
            at: r
                .popup_time(r.selected)
                .min((r.info.duration_secs - 0.05).max(0.0)),
            src_width: r.info.display_width,
            src_height: r.info.display_height,
            tools: r.tools.clone(),
            input: r.input.clone(),
            prefetch: false,
        })
    }

    fn current_key(&self) -> Option<preview::FrameKey> {
        let r = self.review.as_ref()?;
        r.chunk_key(r.selected)
    }

    /// Show the selected popup's frame. The frame is fetched once per popup and
    /// cached; moving between words only rebuilds the caption.
    fn seek_preview(&mut self) {
        let Some(target) = self.preview_target() else {
            return;
        };
        let key = target.key;
        let chunk = target.chunk;

        // Same popup: the frame is already on screen; only the caption changed.
        if self.applied_frame == Some(key) {
            if let Some(r) = self.review.as_mut() {
                r.preview.release_hold();
                r.preview.invalidate();
            }
            return;
        }

        if let Some(frame) = self.frame_cache.get(key) {
            if let Some(r) = self.review.as_mut() {
                r.preview.set_frame(frame);
            }
            self.applied_frame = Some(key);
            self.requested_frame = None;
            self.prefetch_neighbours(chunk);
            return;
        }

        // Already waiting for this popup: the request (and its prefetch) are
        // in flight, and a newer click will coalesce at the worker.
        if self.requested_frame == Some(key) {
            return;
        }

        // Not cached. Keep the old image on screen until the new frame is ready;
        // do not composite the new caption over the previous popup's frame.
        self.requested_frame = Some(key);
        let _ = self.preview_tx.send(target);
        if let Some(r) = self.review.as_mut() {
            r.preview.hold();
        }
        self.prefetch_neighbours(chunk);
    }

    /// Warm the neighbouring popups so Next/Previous and quick back-and-forth
    /// switches hit the cache.
    fn prefetch_neighbours(&mut self, selected: usize) {
        let Some(r) = self.review.as_ref() else {
            return;
        };
        let mut jobs = Vec::new();
        let neighbours = [
            selected.checked_sub(1),
            (selected + 1 < r.chunks.len()).then_some(selected + 1),
        ];
        for idx in neighbours.into_iter().flatten() {
            let Some(key) = r.chunk_key(idx) else {
                continue;
            };
            if self.frame_cache.contains(key)
                || self.applied_frame == Some(key)
                || self.requested_frame == Some(key)
            {
                continue;
            }
            jobs.push(PreviewJob {
                generation: self.preview_generation,
                chunk: idx,
                key,
                at: r
                    .popup_time(idx)
                    .min((r.info.duration_secs - 0.05).max(0.0)),
                src_width: r.info.display_width,
                src_height: r.info.display_height,
                tools: r.tools.clone(),
                input: r.input.clone(),
                prefetch: true,
            });
        }
        for job in jobs {
            let _ = self.preview_tx.send(job);
        }
    }

    /// Export was requested from review: validate, then ask where to save.
    fn request_export(&mut self) {
        if self.pending_overwrite.is_some() {
            return;
        }
        if self.stage != Stage::Review {
            eprintln!("      export ignored: stage is not review");
            return;
        }
        let Some(r) = self.review.as_ref() else {
            eprintln!("      export ignored: no review state");
            return;
        };
        if r.to_chunks().is_empty() {
            self.error = Some("nothing to export: every caption is empty".into());
            return;
        }
        self.pending_pick = Some(PickRequest::SaveExport);
    }

    /// Run the save panel, then start the export (or ask to replace a file).
    fn export_after_pick(&mut self) {
        let Some(r) = self.review.as_ref() else {
            return;
        };
        let chunks = r.to_chunks();
        if chunks.is_empty() {
            self.error = Some("nothing to export: every caption is empty".into());
            return;
        }
        let input = r.input.clone();
        let suggested = output::default_output(&input);
        // The render pass makes one PNG per caption state; the step row shows
        // the count, and it is the same arithmetic the pipeline does.
        let states: usize = chunks.iter().map(|c| c.state_windows().len()).sum();
        let plan = ExportPlan {
            input: r.input.clone(),
            tools: r.tools.clone(),
            info: r.info.clone(),
            chunks,
            style: r.style.clone(),
            states,
        };

        let mut dialog = rfd::FileDialog::new()
            .set_title("Save captioned video")
            .add_filter("MP4 video", &["mp4"]);
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        let start_dir = self
            .saved_prefs
            .last_export_dir
            .as_ref()
            .filter(|dir| dir.is_dir())
            .cloned()
            .or_else(|| {
                input
                    .parent()
                    .filter(|dir| !dir.as_os_str().is_empty())
                    .map(Path::to_path_buf)
            });
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }

        let Some(picked) = dialog.save_file() else {
            // Dialog cancelled: stay in Review with every edit intact.
            return;
        };
        let target = match output::ExportTarget::resolve(&input, Some(&picked), true) {
            Ok(target) => target,
            Err(e) => {
                eprintln!("      export destination rejected: {e:#}");
                self.error = Some(format!("{e:#}"));
                return;
            }
        };

        // Remember the folder as soon as the choice is made, even if the encode
        // later fails: that is where the user will want the next one.
        if let Some(dir) = target.final_path().parent() {
            self.saved_prefs.last_export_dir = Some(dir.to_path_buf());
            self.persist_prefs();
        }

        // The macOS panel has already asked "replace?" for the name the user
        // chose. It cannot know that we appended `.mp4`, so confirm ourselves
        // whenever the resolved path differs from the picked one. On Linux the
        // portal may not prompt at all, so always confirm there.
        let picked_final = picked.as_path() == target.final_path();
        let needs_confirm =
            target.final_path().exists() && (!cfg!(target_os = "macos") || !picked_final);
        if needs_confirm {
            self.pending_overwrite = Some((plan, target));
            return;
        }

        self.begin_export(plan, target);
    }

    /// Start the worker. The encoder writes a hidden part file; only a
    /// successful encode is renamed over the destination.
    fn begin_export(&mut self, plan: ExportPlan, target: output::ExportTarget) {
        let output = target.final_path().to_path_buf();
        self.error = None;
        self.export = Some(ExportStatus {
            file: file_label(&output),
            phase: ExportPhase::Rendering,
            phase_frac: 0.0,
            overall: 0.0,
            states: plan.states,
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
        let dims = (
            plan.info.display_width,
            plan.info.display_height,
            plan.info.duration_secs,
        );
        self.pending += 1;
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            if let Err(e) = target.prepare() {
                target.discard();
                let _ = done_tx.send(Msg::ExportErr(format!("{e:#}")));
                return;
            }
            let encoded = pipeline::export_video(
                &plan.tools,
                &plan.input,
                target.part_path(),
                &plan.info,
                &plan.chunks,
                &plan.style,
                crate::export::Encoder::Auto,
                &cancel,
                sink,
            );
            match encoded.and_then(|()| target.commit()) {
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
                    target.discard();
                    let _ = done_tx.send(Msg::ExportErr(format!("{e:#}")));
                }
            }
        });
    }

    fn service_pick(&mut self) {
        // The overwrite modal owns the interaction while it is open; a stray
        // Cmd+E must not open a native panel behind it.
        if self.pending_overwrite.is_some() {
            self.pending_pick = None;
            return;
        }
        match self.pending_pick.take() {
            Some(PickRequest::OpenVideo) => {
                if let Some(path) = pick_video() {
                    self.start_ingest(path);
                }
            }
            Some(PickRequest::SaveExport) => self.export_after_pick(),
            None => {}
        }
    }

    /// In-app overwrite confirmation. macOS uses this only when the resolved
    /// name differs from the panel's pick (for example, `.mp4` was appended);
    /// Linux always confirms because the portal may not.
    fn confirm_overwrite(&mut self, ctx: &egui::Context) {
        let Some((plan, target)) = self.pending_overwrite.take() else {
            return;
        };
        let path = target.final_path().display().to_string();
        let mut replace = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("replace_export")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label("A file already exists at:");
            ui.add(
                egui::Label::new(egui::RichText::new(&path).monospace().small())
                    .wrap()
                    .selectable(true),
            );
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Replacing it cannot be undone.").weak());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if theme::button(ui, "Replace").clicked() {
                    replace = true;
                }
                if theme::button(ui, "Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if replace {
            self.begin_export(plan, target);
        } else if !cancel {
            self.pending_overwrite = Some((plan, target));
        }
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
        self.pending_pick = None;
        self.pending_overwrite = None;
        // Invalidate in-flight preview work and drop cached frames from the
        // video that is going away.
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.frame_cache.clear();
        self.applied_frame = None;
        self.requested_frame = None;
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
                // New video: discard any frames or in-flight extraction from
                // the previous one.
                self.preview_generation = self.preview_generation.wrapping_add(1);
                self.frame_cache.clear();
                self.applied_frame = None;
                self.requested_frame = None;
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
            Msg::PreviewFrame {
                generation,
                key,
                frame,
            } => {
                if generation != self.preview_generation {
                    return;
                }
                // Any result for the key we are waiting on ends the wait, even
                // if the user has already navigated elsewhere.
                if self.requested_frame == Some(key) {
                    self.requested_frame = None;
                }
                let current = self.current_key();
                match frame {
                    Some(frame) => {
                        if current == Some(key) {
                            self.frame_cache.insert(key, frame.clone());
                            if let Some(r) = self.review.as_mut() {
                                r.preview.set_frame(frame);
                            }
                            self.applied_frame = Some(key);
                        } else {
                            // A prefetch result: cache it without touching the
                            // displayed composite.
                            self.frame_cache.insert(key, frame);
                        }
                    }
                    None => {
                        if current == Some(key) && self.stage == Stage::Review {
                            self.error = Some("frame extraction failed".into());
                        }
                    }
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
            self.request_export();
        } else if seek {
            // Frames are cached by each chunk's time span, so a re-chunk
            // naturally misses the old cache entries.
            self.seek_preview();
            // A cache hit or a caption-only change happens after the panels
            // were drawn; make sure the new composite is painted next frame.
            ui.ctx().request_repaint();
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

        // Native dialogs run after the frame, never inside a draw closure.
        self.service_pick();
        self.confirm_overwrite(&ctx);

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

/// Ask the user for a video with the OS file dialog.
fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a video")
        .add_filter("Video", &["mp4", "m4v", "mov", "webm", "mkv", "avi"])
        .pick_file()
}

/// Open `path` in the OS default application (e.g. the video's player).
fn open_file(path: &Path) -> Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        // No `--`: Ubuntu's xdg-open script rejects unknown options (the same
        // reason platform::reveal_in_file_manager avoids it).
        std::process::Command::new("xdg-open")
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
    use super::widgets::{centred_buttons, column, COLUMN_W};
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
