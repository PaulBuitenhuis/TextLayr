mod audio;
mod chunk;
mod config;
mod export;
mod model;
mod probe;
mod render;
mod transcribe;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::time::Instant;

/// CLI options (parsed by hand — no arg-parsing dependency yet).
#[derive(Debug, Default)]
struct Args {
    input: PathBuf,
    /// Milestone 1 behavior: print the word table and stop.
    words_only: bool,
    /// Ignore (and don't write) the transcript cache.
    no_cache: bool,
    output: Option<PathBuf>,
    /// Render states + filter script into this directory instead of encoding.
    png_dir: Option<PathBuf>,
    style_overrides: StyleOverrides,
}

/// Style knobs exposed on the CLI for fast iteration (they become GUI sliders
/// in Milestone 3).
#[derive(Debug, Default)]
struct StyleOverrides {
    font: Option<String>,
    font_size_pct: Option<f32>,
    position_pct: Option<f32>,
    words_per_popup: Option<usize>,
}

fn usage() -> ! {
    eprintln!("usage: texxy <video.mp4> [options]");
    eprintln!();
    eprintln!("Burns word-synced karaoke captions into a video.");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --words              Milestone 1 mode: print (word, start, end) and stop");
    eprintln!("  --no-cache           Ignore the cached transcript, re-transcribe");
    eprintln!("  -o, --output <path>  Output path (default: <input>_karaoke.mp4)");
    eprintln!("  --png-dir <path>     Render caption states + filter script, don't encode");
    eprintln!("  --font <family>      Libre Baskerville | Inter | Nunito");
    eprintln!("  --font-size <pct>    Font size as % of frame height");
    eprintln!("  --position <pct>     Vertical center of the caption box (% of height)");
    eprintln!("  --words-per <n>      Words per popup (2-6)");
    std::process::exit(2);
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    let mut positional: Option<PathBuf> = None;

    while let Some(arg) = it.next() {
        let mut value =
            |name: &str| it.next().with_context(|| format!("{name} requires a value"));
        match arg.as_str() {
            "-h" | "--help" => usage(),
            "--words" => args.words_only = true,
            "--no-cache" => args.no_cache = true,
            "-o" | "--output" => args.output = Some(PathBuf::from(value("--output")?)),
            "--png-dir" => args.png_dir = Some(PathBuf::from(value("--png-dir")?)),
            "--font" => args.style_overrides.font = Some(value("--font")?),
            "--font-size" => {
                args.style_overrides.font_size_pct = Some(parse_num(&value("--font-size")?)?)
            }
            "--position" => {
                args.style_overrides.position_pct = Some(parse_num(&value("--position")?)?)
            }
            "--words-per" => {
                args.style_overrides.words_per_popup =
                    Some(parse_num::<usize>(&value("--words-per")?)?)
            }
            other if other.starts_with('-') => bail!("unknown option: {other}"),
            other => {
                if positional.is_some() {
                    bail!("unexpected extra argument: {other}");
                }
                positional = Some(PathBuf::from(other));
            }
        }
    }

    let Some(input) = positional else { usage() };
    if !input.is_file() {
        bail!("input file not found: {}", input.display());
    }
    args.input = input;
    Ok(args)
}

fn parse_num<T: std::str::FromStr>(s: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    s.parse()
        .map_err(|e| anyhow::anyhow!("expected a number, got '{s}': {e}"))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let mut style = config::Style::default();
    apply_overrides(&mut style, &args.style_overrides);
    style.sanitize();

    // ── 1. probe ───────────────────────────────────────────────────────────
    eprintln!("[1/4] probing video…");
    let tools = probe::find_ffmpeg().context("ffmpeg discovery failed")?;
    eprintln!("      ffmpeg : {}", tools.ffmpeg.display());
    eprintln!("      ffprobe: {}", tools.ffprobe.display());

    let info = probe::probe(&tools, &args.input).context("probe failed")?;
    eprintln!(
        "      {}x{} (display)  {:.1}s  {:.2} fps  audio={}  pix={}",
        info.display_width,
        info.display_height,
        info.duration_secs,
        info.fps,
        info.has_audio,
        info.pix_fmt
    );
    if !info.has_audio {
        bail!("input has no audio track; cannot transcribe");
    }

    // ── 2. transcribe (cached when possible) ───────────────────────────────
    let words = get_words(&tools, &args)?;

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
    let chunks = chunk::chunk_words(&words, style.words_per_popup);
    if chunks.is_empty() {
        bail!("transcript is empty; nothing to caption");
    }
    eprintln!(
        "      {} words -> {} chunks ({} words/popup)",
        words.len(),
        chunks.len(),
        style.words_per_popup
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
    let states = render::render_states(
        &style,
        &chunks,
        info.display_width,
        info.display_height,
        &state_dir,
    )?;
    eprintln!(
        "      {} state PNGs in {:.1}s",
        states.len(),
        t.elapsed().as_secs_f64()
    );

    if let Some(dir) = &args.png_dir {
        // Debug mode: emit the graph + windows, skip encoding.
        let script = dir.join("filter_complex.txt");
        export::write_filter_script(&states, style.position_pct, &script)?;
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
        eprintln!("      wrote {} + states.txt to {}", script.display(), dir.display());
        return Ok(());
    }

    // ── 4. export ──────────────────────────────────────────────────────────
    let output = args.output.clone().unwrap_or_else(|| default_output(&args.input));
    eprintln!("[4/4] encoding with ffmpeg…");
    let t = Instant::now();
    let script = state_dir.join("filter_complex.txt");
    let mut last_pct = 0u32;
    export::export(
        &tools,
        &args.input,
        &output,
        &info,
        &states,
        style.position_pct,
        &script,
        |frac| {
            let pct = (frac * 100.0) as u32;
            if pct != last_pct && pct % 5 == 0 {
                last_pct = pct;
                eprint!("\r      encode {pct:3}%");
            }
        },
    )?;
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

fn apply_overrides(style: &mut config::Style, o: &StyleOverrides) {
    if let Some(f) = &o.font {
        style.font_family = f.clone();
    }
    if let Some(v) = o.font_size_pct {
        style.font_size_pct = v;
    }
    if let Some(v) = o.position_pct {
        style.position_pct = v;
    }
    if let Some(v) = o.words_per_popup {
        style.words_per_popup = v;
    }
}

fn default_output(input: &PathBuf) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string());
    let name = format!("{stem}_karaoke.mp4");
    match input.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

/// Transcribed words, from cache when the input file is unchanged.
fn get_words(tools: &probe::Fftools, args: &Args) -> Result<Vec<transcribe::Word>> {
    if !args.no_cache {
        if let Some(cached) = transcribe::load_cached(&args.input) {
            eprintln!(
                "[2/4] transcript cache hit: {} words ({})",
                cached.len(),
                transcribe::cache_path(&args.input)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
            return Ok(cached);
        }
    }
    eprintln!("[2/4] extracting audio + transcribing with whisper small.en…");
    let tmp = tempfile::tempdir().context("temp dir")?;
    let pcm_path = tmp.path().join("audio.f32le");
    let t = Instant::now();
    audio::extract_pcm(tools, &args.input, &pcm_path).context("audio extraction failed")?;
    let samples = audio::read_pcm(&pcm_path).context("PCM read failed")?;
    let audio_secs = samples.len() as f64 / 16000.0;
    eprintln!("      {:.1}s of audio extracted in {:.1}s", audio_secs, t.elapsed().as_secs_f64());

    let model = model::find_model().context("model discovery failed")?;
    eprintln!(
        "      model: {} ({:.0} MB)",
        model.display(),
        model::file_size(&model)? as f64 / 1e6
    );
    let t = Instant::now();
    let words = transcribe::transcribe(&model, &samples).context("transcription failed")?;
    let elapsed = t.elapsed().as_secs_f64();
    eprintln!(
        "      {} words in {:.1}s  ({:.1}x realtime)",
        words.len(),
        elapsed,
        if elapsed > 0.0 { audio_secs / elapsed } else { 0.0 }
    );
    if let Err(e) = transcribe::save_cache(&args.input, &words) {
        eprintln!("      (cache write skipped: {e:#})");
    }
    Ok(words)
}
