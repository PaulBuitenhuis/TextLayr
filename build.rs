//! Stamp the build with the short git hash so the UI (and window title) can
//! show exactly which revision is running. This matters because a fixed bug can
//! look "still broken" when an older binary is what's actually on screen.

fn main() {
    // Re-run when the current commit changes: HEAD points at a branch, whose
    // ref (and reflog) moves on commit/checkout/rw.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");

    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "no-git".to_string());
    println!("cargo:rustc-env=TEXTLAYR_GIT_HASH={hash}");
}
