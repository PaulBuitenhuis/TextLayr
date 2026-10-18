#!/usr/bin/env pwsh
#
# Build a portable Windows release of TextLayr (x64).
#
# Output:
#   dist/TextLayr-<version>-windows-x64.zip
#
# The archive bundles the GUI binary (textlayr.exe), the console binary
# (textlayr-cli.exe), the caption fonts under resources\assets\fonts (where the
# app looks for bundled resources on Windows), the LICENSE and a short README.
# ffmpeg/ffprobe and the whisper model are downloaded on first run.
#
# Usage:
#   pwsh scripts/package_windows.ps1
#
# Env:
#   TEXTLAYR_VERSION   version string (default: the version in Cargo.toml)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Say($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Die($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

# ── 1. release build ────────────────────────────────────────────────────────
Say "building release binaries"
cargo build --release --bins

$bin = Join-Path $repoRoot "target\release\textlayr.exe"
$cli = Join-Path $repoRoot "target\release\textlayr-cli.exe"
if (-not (Test-Path $bin)) { Die "release binary not found at $bin" }
if (-not (Test-Path $cli)) { Die "console binary not found at $cli" }

# ── 2. version ──────────────────────────────────────────────────────────────
$version = $env:TEXTLAYR_VERSION
if (-not $version) {
    $m = Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"(?<v>[^"]+)"'
    if ($m) { $version = $m.Matches[0].Groups["v"].Value }
}
if (-not $version) { Die "could not determine a version (set TEXTLAYR_VERSION)" }
$version = $version.TrimStart("v")

# ── 3. assemble ─────────────────────────────────────────────────────────────
$name = "TextLayr-$version-windows-x64"
$stage = Join-Path $repoRoot "dist\$name"
Say "assembling $stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path "$stage\resources\assets\fonts" | Out-Null

Copy-Item $bin "$stage\textlayr.exe"
Copy-Item $cli "$stage\textlayr-cli.exe"
Copy-Item "$repoRoot\assets\fonts\*.ttf" "$stage\resources\assets\fonts\"
if (Test-Path "$repoRoot\assets\fonts\OFL.txt") {
    Copy-Item "$repoRoot\assets\fonts\OFL.txt" "$stage\resources\assets\fonts\"
}
Copy-Item "$repoRoot\LICENSE" "$stage\LICENSE"

$readme = @"
TextLayr $version - Windows (x64)

  textlayr.exe      the app (double-click to open)
  textlayr-cli.exe  console version (for scripts)

First run downloads ffmpeg + a whisper model. Either launch textlayr.exe and
follow the Setup screen, or, from this folder, run:

  .\textlayr-cli.exe --setup

This build is not code-signed, so Windows SmartScreen may warn on first launch
(choose More info -> Run anyway).
"@
Set-Content -Path "$stage\README.txt" -Value $readme -Encoding UTF8

# ── 4. zip ──────────────────────────────────────────────────────────────────
$zip = Join-Path $repoRoot "dist\$name.zip"
Say "archiving $zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip

# ── 5. report ───────────────────────────────────────────────────────────────
Say ("done: {0} ({1:N1} MB)" -f $zip, ((Get-Item $zip).Length / 1MB))
