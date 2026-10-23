//! Bounded helpers for one-shot child processes.
//!
//! ffmpeg/ffprobe normally answer immediately, but a wedged GPU driver or a
//! stalled mount can leave a probe blocked forever. These helpers spawn, poll
//! with a short interval, and always kill + reap on timeout.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// Probes are one-shot: they should never take anywhere near this long.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

const POLL: Duration = Duration::from_millis(10);

/// Run `cmd`, capturing stdout/stderr, and fail if it outlives `timeout`.
pub fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("failed to spawn a helper process")?;
    let mut stdout = child.stdout.take().context("helper stdout pipe")?;
    let mut stderr = child.stderr.take().context("helper stderr pipe")?;
    // Drain both pipes while waiting: a full stderr pipe would otherwise block
    // the child and make the timeout fire for the wrong reason.
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let status = match wait_with_timeout(&mut child, timeout) {
        Ok(status) => status,
        Err(e) => {
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err(e);
        }
    };
    Ok(Output {
        status,
        stdout: out_thread.join().unwrap_or_default(),
        stderr: err_thread.join().unwrap_or_default(),
    })
}

/// Run `cmd`, discarding its output, and fail if it outlives `timeout`.
pub fn status_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<ExitStatus> {
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("failed to spawn a helper process")?;
    wait_with_timeout(&mut child, timeout)
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to poll a helper process")?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("a helper process timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_fast_command_returns_its_output() {
        let out = output_with_timeout(
            Command::new("/bin/sh").arg("-c").arg("printf hello"),
            PROBE_TIMEOUT,
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_command_is_killed() {
        let started = Instant::now();
        let err = status_with_timeout(
            Command::new("/bin/sh").arg("-c").arg("sleep 60"),
            Duration::from_millis(150),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
