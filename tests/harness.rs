//! Harness self-tests and the headless end-to-end export.
//!
//! These use the same tool discovery the app uses (`probe::find_ffmpeg`), so
//! they run against `vendor/<platform>/` in a dev checkout, a `PATH` install in
//! CI, or a fixture directory via `$TEXTLAYR_FFTOOLS`. Anything that needs a
//! *real* ffmpeg skips (with a printed reason) when none can be found, so a
//! machine without one still gets the fixture self-tests.
//!
//! Why this exists: the render → encode path used to have no automated coverage
//! at all, because it needs a whisper model. `--transcript` removes that
//! dependency, so an export can be driven from a fixed word list.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use textlayr::probe::Fftools;

/// The CLI built for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_textlayr");

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_tool(scenario: &str, tool: &str) -> PathBuf {
    manifest_dir()
        .join("tests/fixtures/fftools")
        .join(scenario)
        .join(tool)
}

fn fixture_transcript() -> PathBuf {
    manifest_dir().join("tests/fixtures/transcript-2s.json")
}

fn fonts_dir() -> PathBuf {
    manifest_dir().join("assets/fonts")
}

/// Real tools, if any. Skipped when the process itself was pointed at fixture
/// tools — running the end-to-end export against a 100-byte fake decoder would
/// only produce a confusing failure.
fn real_tools() -> Option<Fftools> {
    if std::env::var_os("TEXTLAYR_FFTOOLS").is_some() {
        eprintln!("skipping: $TEXTLAYR_FFTOOLS is set in the test environment");
        return None;
    }
    match textlayr::probe::find_ffmpeg() {
        Ok(tools) => Some(tools),
        Err(e) => {
            eprintln!("skipping: no real ffmpeg available ({e:#})");
            None
        }
    }
}

/// A 2 s 320x568 clip with a tone, so probing, caption rendering and encoding
/// all have something real to work on.
fn synth_clip(tools: &Fftools, dir: &Path) -> PathBuf {
    let out = dir.join("clip.mp4");
    let status = Command::new(&tools.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x203040:s=320x568:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&out)
        .status()
        .expect("could not spawn the synthesizing ffmpeg");
    assert!(status.success(), "could not synthesize the fixture clip");
    out
}

fn assert_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("fixture {} is missing: {e}", path.display()))
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "{} must be executable — run `chmod +x tests/fixtures/fftools/*/*`",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// Fixture self-tests: a later flaky result must be the fix's fault, not the
// harness's.
// ---------------------------------------------------------------------------

#[test]
fn the_hang_fixture_really_hangs() {
    for tool in ["ffmpeg", "ffprobe"] {
        let path = fixture_tool("hang", tool);
        assert_executable(&path);
        let mut child = Command::new(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn hang fixture");

        std::thread::sleep(Duration::from_millis(300));
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "{tool} fixture exited; it must stay alive to exercise a timeout"
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[test]
fn the_fail_fixture_reports_a_recognisable_failure() {
    for tool in ["ffmpeg", "ffprobe"] {
        let path = fixture_tool("fail", tool);
        assert_executable(&path);
        let out = Command::new(&path).output().expect("run fail fixture");
        assert_eq!(out.status.code(), Some(3), "{tool} should exit 3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("textlayr-fixture:"),
            "{tool} stderr: {stderr}"
        );
    }
}

#[test]
fn the_torn_frame_fixture_writes_part_of_a_frame() {
    let path = fixture_tool("torn-frame", "ffmpeg");
    assert_executable(&path);
    let out = Command::new(&path)
        .output()
        .expect("run torn-frame fixture");
    assert!(out.status.success(), "the fixture should claim success");
    assert_eq!(
        out.stdout.len(),
        100,
        "the fixture must write a partial frame, not a whole one"
    );
}

// ---------------------------------------------------------------------------
// End-to-end: discovery, transcript injection and export.
// ---------------------------------------------------------------------------

#[test]
fn the_override_directory_drives_the_cli() {
    let Some(tools) = real_tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let clip = synth_clip(&tools, work.path());
    let log = work.path().join("tools.log");

    let out = Command::new(BIN)
        .arg(&clip)
        .args(["--words", "--transcript"])
        .arg(fixture_transcript())
        .env(
            "TEXTLAYR_FFTOOLS",
            manifest_dir().join("tests/fixtures/fftools/pass"),
        )
        .env("TEXTLAYR_REAL_FFMPEG", &tools.ffmpeg)
        .env("TEXTLAYR_REAL_FFPROBE", &tools.ffprobe)
        .env("TEXTLAYR_FFTOOLS_LOG", &log)
        .env("TEXTLAYR_FONTS", fonts_dir())
        .output()
        .expect("run the CLI");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Make"), "word table: {stdout}");
    assert!(stdout.contains("count"), "word table: {stdout}");

    // The fixture wrapper logged the invocation, which proves discovery used
    // $TEXTLAYR_FFTOOLS rather than the vendored or PATH copy.
    let log = std::fs::read_to_string(&log).expect("fixture tools were not invoked");
    assert!(log.contains("ffprobe "), "tools log: {log}");
}

#[test]
fn transcript_is_rejected_with_gui() {
    // Parsed and rejected before any dependency check, so this needs no tools.
    let out = Command::new(BIN)
        .args(["--gui", "--transcript"])
        .arg(fixture_transcript())
        .output()
        .expect("run the CLI");
    assert_eq!(out.status.code(), Some(2), "expected a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--transcript is headless-only"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_transcript_run_exports_without_a_model() {
    let Some(tools) = real_tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let clip = synth_clip(&tools, work.path());
    let output = work.path().join("out.mp4");

    let out = Command::new(BIN)
        .arg(&clip)
        .args(["--headless", "--transcript"])
        .arg(fixture_transcript())
        .arg("-o")
        .arg(&output)
        .args(["--encoder", "x264"])
        .env(
            "TEXTLAYR_FFTOOLS",
            manifest_dir().join("tests/fixtures/fftools/pass"),
        )
        .env("TEXTLAYR_REAL_FFMPEG", &tools.ffmpeg)
        .env("TEXTLAYR_REAL_FFPROBE", &tools.ffprobe)
        .env("TEXTLAYR_FONTS", fonts_dir())
        .output()
        .expect("run the CLI");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "export failed: {stderr}");
    assert!(
        stderr.contains("x264"),
        "encoder should be reported: {stderr}"
    );

    let size = std::fs::metadata(&output)
        .unwrap_or_else(|e| panic!("no output at {}: {e}", output.display()))
        .len();
    assert!(size > 1_000, "output is suspiciously small ({size} bytes)");

    // The exported file must still be a playable 2 s clip with both streams.
    let probe = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()
        .expect("run ffprobe on the export");
    assert!(probe.status.success(), "ffprobe rejected the export");
    let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let duration: f64 = json["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .expect("duration in probe output");
    assert!(
        (1.5..=2.5).contains(&duration),
        "exported duration {duration} is not the 2 s source"
    );
    let kinds: Vec<&str> = json["streams"]
        .as_array()
        .expect("streams")
        .iter()
        .filter_map(|s| s["codec_type"].as_str())
        .collect();
    assert!(kinds.contains(&"video"), "streams: {kinds:?}");
    assert!(kinds.contains(&"audio"), "streams: {kinds:?}");
}
