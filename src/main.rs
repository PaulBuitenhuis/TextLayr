//! TextLayr binary: dispatches between the GUI and the headless CLI.
//!
//! - `textlayr` → GUI (drop screen)
//! - `textlayr video.mp4` → GUI with the video pre-loaded
//! - `textlayr video.mp4 --headless` (or any of `--words`/`-o`/`--png-dir`/style
//!   flags) → the Milestone 1–2 headless pipeline

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};

use textlayr::config::{Grouping, PopupEffect, Style};
use textlayr::install::{self, InstallProgress};
use textlayr::model::Model;
use textlayr::pipeline::{self, IngestProgress, Progress};
use textlayr::{chunk, deps, export, profile, render, transcribe, ui, update};

/// CLI options (parsed by hand — no arg-parsing dependency).
#[derive(Debug, Default)]
struct Args {
    /// Video to open in the GUI, or to process when headless.
    input: Option<PathBuf>,
    /// Force the GUI even when headless-style flags are present.
    gui: bool,
    /// Run the pipeline without a GUI.
    headless: bool,
    /// Download the missing dependencies (ffmpeg/model) and stop.
    setup: bool,
    /// Check GitHub for a newer release, print the result, and stop.
    check_updates: bool,
    /// Milestone 1 behavior: print the word table and stop (implies headless).
    words_only: bool,
    /// Ignore (and don't write) the transcript cache.
    no_cache: bool,
    output: Option<PathBuf>,
    /// Render states + filter script into this directory instead of encoding.
    png_dir: Option<PathBuf>,
    /// Milestone 2 backend: encode via the N-stage overlay filter graph.
    graph: bool,
    /// Hardware/software encoder for the default pipe export.
    encoder: Option<export::Encoder>,
    /// Whisper model to use (base.en/small.en); defaults to the fastest.
    model: Option<Model>,
    /// Built-in or user profile id/name to start from (headless).
    profile: Option<String>,
    /// Print available profiles and exit.
    list_profiles: bool,
    style_overrides: StyleOverrides,
}

/// Style knobs exposed on the CLI for fast iteration (the GUI exposes them as
/// sliders and pickers).
#[derive(Debug, Default)]
struct StyleOverrides {
    font: Option<String>,
    font_size_pct: Option<f32>,
    position_pct: Option<f32>,
    words_per_popup: Option<usize>,
}

impl StyleOverrides {
    fn any(&self) -> bool {
        self.font.is_some()
            || self.font_size_pct.is_some()
            || self.position_pct.is_some()
            || self.words_per_popup.is_some()
    }
}

fn usage() -> ! {
    eprintln!("usage: textlayr [video.mp4] [options]");
    eprintln!();
    eprintln!("Burns word-synced karaoke captions into a video.");
    eprintln!("Without headless options the GUI opens (a video argument is pre-loaded).");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --gui               Open the GUI (default when no other options)");
    eprintln!("  --headless          Run the pipeline without a GUI");
    eprintln!(
        "  --setup             Download missing ffmpeg/model into the app data dir, then exit"
    );
    eprintln!("  --check-updates     Check GitHub for a newer TextLayr release, then exit");
    eprintln!("  --words             Milestone 1 mode: print (word, start, end) and stop");
    eprintln!("  --no-cache          Ignore the cached transcript, re-transcribe");
    eprintln!("  -o, --output <path> Output path (default: <input>_textlayr.mp4)");
    eprintln!("  --png-dir <path>    Render caption states + filter script, don't encode");
    eprintln!(
        "  --encoder <name>    auto (default) | vaapi | videotoolbox | x264 — pipe export encoder"
    );
    eprintln!("  --model <name>      base.en (default, fast) | small.en (more accurate)");
    eprintln!(
        "  --profile <id|path> Style profile: builtin:classic, user:my-look, or a .json file"
    );
    eprintln!("  --list-profiles     List built-in and saved style profiles, then exit");
    eprintln!("  --graph             Legacy overlay-graph encode (RAM grows; no motion)");
    eprintln!("  --font <family>     Libre Baskerville | Inter | Nunito");
    eprintln!("  --font-size <pct>   Font size as % of frame height");
    eprintln!("  --position <pct>    Vertical center of the caption box (% of height)");
    eprintln!("  --words-per <n>     Words per popup (1-8; 1 = word by word)");
    eprintln!("  -V, --version       Print the version and build hash, then exit");
    std::process::exit(2);
}

/// `--version` / `-V`: the version plus the short build hash, so a user (or a
/// bug report) can tell exactly which revision is running.
fn version() -> ! {
    println!(
        "textlayr {} (build {})",
        env!("CARGO_PKG_VERSION"),
        env!("TEXTLAYR_GIT_HASH")
    );
    std::process::exit(0);
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    let mut positional: Option<PathBuf> = None;

    // Usage errors exit right here: this is a CLI, not a library.
    fn need_value(it: &mut impl Iterator<Item = String>, name: &str) -> String {
        match it.next() {
            Some(v) => v,
            None => {
                eprintln!("error: {name} requires a value");
                std::process::exit(2);
            }
        }
    }

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => usage(),
            "-V" | "--version" => version(),
            "--gui" => args.gui = true,
            "--headless" => args.headless = true,
            "--setup" => args.setup = true,
            "--check-updates" => args.check_updates = true,
            "--words" => args.words_only = true,
            "--no-cache" => args.no_cache = true,
            "-o" | "--output" => args.output = Some(PathBuf::from(need_value(&mut it, "--output"))),
            "--png-dir" => args.png_dir = Some(PathBuf::from(need_value(&mut it, "--png-dir"))),
            "--graph" => args.graph = true,
            "--model" => {
                let v = need_value(&mut it, "--model");
                args.model = Some(match v.as_str() {
                    "base.en" | "base" => Model::BaseEn,
                    "small.en" | "small" => Model::SmallEn,
                    other => {
                        eprintln!("error: --model expects base.en or small.en, got '{other}'");
                        std::process::exit(2);
                    }
                });
            }
            "--encoder" => {
                let v = need_value(&mut it, "--encoder");
                args.encoder = Some(match v.as_str() {
                    "auto" => export::Encoder::Auto,
                    "x264" => export::Encoder::X264,
                    "vaapi" => export::Encoder::Vaapi,
                    "videotoolbox" | "vt" => export::Encoder::Videotoolbox,
                    other => {
                        eprintln!(
                            "error: --encoder expects auto, vaapi, videotoolbox or x264, got '{other}'"
                        );
                        std::process::exit(2);
                    }
                });
            }
            "--profile" | "--preset" => {
                args.profile = Some(need_value(&mut it, "--profile"));
            }
            "--list-profiles" | "--list-presets" => args.list_profiles = true,
            "--font" => args.style_overrides.font = Some(need_value(&mut it, "--font")),
            "--font-size" => {
                let v = need_value(&mut it, "--font-size");
                args.style_overrides.font_size_pct = Some(parse_num(&v, "--font-size"));
            }
            "--position" => {
                let v = need_value(&mut it, "--position");
                args.style_overrides.position_pct = Some(parse_num(&v, "--position"));
            }
            "--words-per" => {
                let v = need_value(&mut it, "--words-per");
                args.style_overrides.words_per_popup = Some(parse_num(&v, "--words-per"));
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                std::process::exit(2);
            }
            other => {
                if positional.is_some() {
                    eprintln!("error: unexpected extra argument: {other}");
                    std::process::exit(2);
                }
                positional = Some(PathBuf::from(other));
            }
        }
    }

    args.input = positional;
    if let Some(input) = &args.input {
        if !input.is_file() {
            eprintln!("error: input file not found: {}", input.display());
            std::process::exit(2);
        }
    }
    args
}

fn parse_num<T: std::str::FromStr>(s: &str, flag: &str) -> T
where
    T::Err: std::fmt::Display,
{
    s.parse().unwrap_or_else(|e| {
        eprintln!("error: {flag} expects a number, got '{s}': {e}");
        std::process::exit(2);
    })
}

fn main() {
    // Adopt any pre-rename (`texxy`) data/config/cache before anything reads or
    // creates those directories.
    textlayr::migrate_legacy_dirs();

    let args = parse_args();

    // `--check-updates` is its own mode: ask GitHub, report, then exit.
    if args.check_updates {
        run_check_updates();
    }

    // `--setup` is its own mode: fetch what's missing, then exit.
    if args.setup {
        if let Err(e) = run_setup(&args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    // Listing profiles needs neither a video nor any dependencies.
    if args.list_profiles {
        for line in profile::list_lines() {
            println!("{line}");
        }
        return;
    }

    // Any output/style option implies the headless pipeline; --gui wins over
    // that so `textlayr --gui --font Inter` can still open the GUI.
    let headless = !args.gui
        && (args.headless
            || args.words_only
            || args.output.is_some()
            || args.png_dir.is_some()
            || args.graph
            || args.encoder.is_some()
            || args.model.is_some()
            || args.profile.is_some()
            || args.style_overrides.any());

    let result = if headless {
        run_cli(&args)
    } else {
        ui::run(args.input.clone())
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// `--check-updates`: ask GitHub for the newest release and report whether this
/// build is behind. Exit 0 when up to date, 10 when an update is available, and
/// 1 on error (so scripts can distinguish "behind" from "couldn't tell").
fn run_check_updates() -> ! {
    let cancel = AtomicBool::new(false);
    match update::fetch_latest(&cancel) {
        Ok(release) => {
            println!(
                "current: {} (build {})",
                update::current_version(),
                env!("TEXTLAYR_GIT_HASH")
            );
            println!("latest:  {}  {}", release.version, release.notes_url);
            if update::forced() || update::is_newer(update::current_version(), &release.version) {
                println!("an update is available");
                std::process::exit(10);
            }
            println!("up to date");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

/// `--setup`: download every missing dependency (ffmpeg/ffprobe, the model)
/// into the platform data directory, reporting progress on stderr. Exits
/// non-zero if anything is still missing afterwards.
fn run_setup(args: &Args) -> Result<()> {
    let preferred = args.model.unwrap_or_default();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut last_pct = u32::MAX;
    let sink: install::InstallSink = Box::new(move |p| match p {
        InstallProgress::NothingToDo => eprintln!("everything is already installed"),
        InstallProgress::Begin {
            index,
            total,
            label,
        } => {
            eprintln!("[{}/{}] downloading {label}…", index + 1, total);
        }
        InstallProgress::Bytes { got, expected, .. } => {
            if let Some(total) = expected.filter(|t| *t > 0) {
                let pct = ((got * 100) / total) as u32;
                if pct / 5 != last_pct / 5 {
                    last_pct = pct;
                    eprint!("\r      {pct:3}%");
                }
            }
        }
        InstallProgress::Installed { label, .. } => {
            last_pct = u32::MAX;
            eprintln!("\r      installed {label}          ");
        }
    });

    install::install(preferred, sink, cancel).context("setup failed")?;

    let check = deps::Check::run(preferred);
    if let Some(msg) = check.error_message() {
        bail!("{msg}");
    }
    eprintln!("setup complete");
    Ok(())
}

/// The Milestone 1–2 pipeline, driven from the command line.
fn run_cli(args: &Args) -> Result<()> {
    let Some(input) = args.input.clone() else {
        usage()
    };

    // Resolve the model first: whether it's present decides the dependency
    // check.
    let preferred = args.model.unwrap_or_default();

    // Fail fast with one actionable message instead of discovering each
    // missing dependency (ffmpeg, model, fonts) partway through the pipeline.
    let check = deps::Check::run(preferred);
    if let Some(msg) = check.error_message() {
        bail!("{msg}");
    }
    let found_model = check.model.clone().context("model discovery failed")?;
    let model_kind = found_model.model;

    let mut style = Style::default();
    let mut grouping = Grouping::default();
    if let Some(spec) = &args.profile {
        let store = profile::ProfileStore::new();
        let p = store
            .find(spec)
            .with_context(|| format!("profile not found: {spec}"))?;
        eprintln!("      profile: {} ({})", p.name, p.id);
        style = p.style;
        if let Some(g) = p.grouping {
            grouping = g;
        }
    }
    apply_overrides(&mut style, &mut grouping, &args.style_overrides);
    style.sanitize();
    grouping.sanitize();
    if args.graph && style.motion.popup != PopupEffect::None {
        bail!("motion effects require the default pipe encoder; remove --graph");
    }

    // Phase logging on stderr, shaped like the Milestone 2 output.
    let log_input = input.clone();
    let progress: Progress = Box::new(move |event| match event {
        IngestProgress::Tools(tools) => {
            eprintln!("[1/4] probing video…");
            eprintln!("      ffmpeg : {}", tools.ffmpeg.display());
            eprintln!("      ffprobe: {}", tools.ffprobe.display());
        }
        IngestProgress::Model(m) => eprintln!("      model  : {}", m.id()),
        IngestProgress::Probing => {}
        IngestProgress::Probed(info) => eprintln!(
            "      {}x{} (display)  {:.1}s  {:.2} fps  audio={}  pix={}",
            info.display_width,
            info.display_height,
            info.duration_secs,
            info.fps,
            info.has_audio,
            info.pix_fmt
        ),
        IngestProgress::Cached(n) => eprintln!(
            "[2/4] transcript cache hit: {n} words ({})",
            transcribe::cache_path(&log_input, model_kind)
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
        IngestProgress::Extracting => {
            eprintln!(
                "[2/4] extracting audio + transcribing with whisper {}…",
                model_kind.id()
            )
        }
        IngestProgress::Transcribing(_) => {}
        IngestProgress::Transcribed {
            words,
            secs,
            realtime_x,
        } => eprintln!("      {words} words in {secs:.1}s  ({realtime_x:.1}x realtime)"),
    });

    let ing =
        pipeline::ingest(&input, args.no_cache, &found_model, progress).context("ingest failed")?;
    let tools = ing.tools;
    let info = ing.info;
    let words = ing.words;

    if args.words_only {
        eprintln!();
        eprintln!("=== transcript (word, start, end) ===");
        for w in &words {
            println!("{:>8.2} {:>8.2}  {}", w.start_secs, w.end_secs, w.text);
        }
        return Ok(());
    }

    // ── 3. chunk + render caption states ───────────────────────────────────
    eprintln!("[3/4] rendering captions…");
    let chunks = chunk::chunk_words(&words, grouping.words_per_popup);
    if chunks.is_empty() {
        bail!("transcript is empty; nothing to caption");
    }
    eprintln!(
        "      {} words -> {} chunks ({} words/popup)",
        words.len(),
        chunks.len(),
        grouping.words_per_popup
    );

    let temp = match &args.png_dir {
        Some(_) => None,
        None => Some(tempfile::tempdir().context("temp dir")?),
    };
    let state_dir = args
        .png_dir
        .clone()
        .unwrap_or_else(|| temp.as_ref().unwrap().path().to_path_buf());

    let t = Instant::now();
    // The CLI has no cancel affordance: signal that can never be raised.
    let no_cancel = AtomicBool::new(false);
    let states = render::render_states(
        &style,
        &chunks,
        info.display_width,
        info.display_height,
        &state_dir,
        &no_cancel,
        |_| {},
    )?;
    eprintln!(
        "      {} state PNGs in {:.1}s",
        states.len(),
        t.elapsed().as_secs_f64()
    );

    if let Some(dir) = &args.png_dir {
        // Debug mode: emit the graph + windows, skip encoding.
        let script = dir.join("filter_complex.txt");
        export::write_filter_script(&states, style.layout.position_pct, &script)?;
        let mut windows = String::new();
        for (i, s) in states.iter().enumerate() {
            windows.push_str(&format!(
                "{i:4}  {:>8.3} {:>8.3}  {}\n",
                s.start_secs,
                s.end_secs,
                s.png.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        std::fs::write(dir.join("states.txt"), windows).context("write states.txt")?;
        eprintln!(
            "      wrote {} + states.txt to {}",
            script.display(),
            dir.display()
        );
        return Ok(());
    }

    // ── 4. export ──────────────────────────────────────────────────────────
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| pipeline::default_output(&input));
    eprintln!("[4/4] encoding with ffmpeg…");
    let t = Instant::now();
    let mut last_pct = 0u32;
    let mut on_frac = |frac: f64| {
        let pct = (frac * 100.0) as u32;
        if pct != last_pct && pct.is_multiple_of(5) {
            last_pct = pct;
            eprint!("\r      encode {pct:3}%");
        }
    };
    if args.graph {
        // Milestone 2 backend: one overlay stage per state, exact PTS.
        let script = state_dir.join("filter_complex.txt");
        export::export(
            &tools,
            &input,
            &output,
            &info,
            &states,
            style.layout.position_pct,
            &script,
            &mut on_frac,
        )?;
    } else {
        export::export_pipe(
            &tools,
            &input,
            &output,
            &info,
            &states,
            style.layout.position_pct,
            &style.motion,
            args.encoder.unwrap_or_default(),
            &no_cancel,
            &mut on_frac,
        )?;
    }
    eprintln!("\r      encode 100%");

    let size = std::fs::metadata(&output)
        .map(|m| m.len() as f64 / 1e6)
        .unwrap_or(0.0);
    eprintln!(
        "done in {:.1}s -> {} ({size:.1} MB)",
        t.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

fn apply_overrides(style: &mut Style, grouping: &mut Grouping, o: &StyleOverrides) {
    if let Some(f) = &o.font {
        style.font.family = f.clone();
    }
    if let Some(v) = o.font_size_pct {
        style.font.size_pct = v;
    }
    if let Some(v) = o.position_pct {
        style.layout.position_pct = v;
    }
    if let Some(v) = o.words_per_popup {
        grouping.words_per_popup = v;
    }
}
