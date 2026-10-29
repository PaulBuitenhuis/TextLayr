# Security Policy

TextLayr is a self-contained, offline desktop app: it makes no network requests
after the one-time setup download, has no accounts, no telemetry, and runs no
server. That keeps the attack surface small, but it is not zero — this document
describes what is worth reporting and how.

## Supported versions

Only the **latest release** is supported. TextLayr is pre-1.0, so fixes ship as a
new release rather than as backports to older versions.

| Version | Supported |
|---|---|
| latest release | ✅ |
| older | ❌ |

## Reporting a vulnerability

Please report privately through GitHub, **not** a public issue:

**[Report a vulnerability →](https://github.com/PaulBuitenhuis/TextLayr/security/advisories/new)**
(repo **Security** tab → *Report a vulnerability*). If it is already enabled,
this opens a private advisory only you and the maintainer can see.

Helpful to include:

- affected version + build (`textlayr --version`, or Settings → About),
- your OS and CPU — the macOS version and chip, or your Linux distro and CPU,
- a description of the impact and a minimal way to reproduce it,
- a sample file, if the issue depends on a specific input.

## Scope

### In scope

- **First-run download integrity.** ffmpeg/ffprobe (from `osxexperts.net` on
  macOS, pinned BtbN FFmpeg builds on Linux) and whisper models (from Hugging
  Face) are downloaded in-process and verified against pinned SHA-256 hashes
  before use. Anything that bypasses the checksum, leaves a partial download
  trusted, or resolves the wrong artifact is in scope.
- **Untrusted media handling.** TextLayr feeds arbitrary user video/audio to
  `ffprobe`, `ffmpeg`, and whisper.cpp. A crafted input that escapes the
  intended processing — e.g. via unsafe handling on TextLayr's side — is in scope.
- **Subprocess invocation.** TextLayr invokes `ffmpeg`, `ffprobe`, `open` /
  `xdg-open`, and — on macOS, to swap the app bundle — `ditto` and `xattr`
  directly (no shell). A crafted file path or value that changes the arguments
  TextLayr passes to those tools is in scope.
- **Dependency chain.** Vulnerabilities in TextLayr's own Rust dependencies, and in
  the vendored `whisper-rs-sys` patch (`vendor/patch/whisper-rs-sys`), that are
  exploitable through TextLayr.

### Out of scope

- Bugs in upstream `ffmpeg`, `whisper.cpp`, or the downloaded model files that
  are **not** reachable through TextLayr — please report those upstream. (If TextLayr
  mishandles their output or passes them something dangerous, that *is* ours.)
- Issues that already require code execution or admin rights on the machine, a
  root-compromised `PATH`, or a malicious `$TEXTLAYR_MODEL` file you placed
  yourself.
- The app being unsigned/not notarized (a known, documented trade-off — see the
  README), or Gatekeeper prompts on first launch.

## Security-relevant design

- **No network after setup.** Only the first-run installer downloads anything;
  every request is checksum-pinned.
- **No shell.** External tools are spawned with explicit argument vectors.
- **Nothing redistributed.** `ffmpeg`/`ffprobe` (GPL) and the models are
  downloaded by your machine and invoked as separate programs; TextLayr links and
  ships none of them.
- **Minimal dependencies**, audited in CI by `cargo audit` (see
  `.github/workflows/ci.yml`).

## Disclosure

Reports are handled on a best-effort basis by a single maintainer. You will get
an acknowledgement in the advisory thread, and fixes are released once a
remediation is ready. Please keep the details private until a fix (or a
mitigation) has shipped; you will be credited unless you prefer otherwise.
