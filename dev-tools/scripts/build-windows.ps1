# One-click Windows release build (unsigned).
# Produces target\release\windows_release with the NSIS installer (.exe), the
# MSI, a portable ZIP and the standalone tool executables.
#
# Usage (from anywhere):  powershell -ExecutionPolicy Bypass -File dev-tools\scripts\build-windows.ps1
# Options:  -SkipInstall  skip pnpm installs   -Arm64  build for aarch64-pc-windows-msvc
param([switch]$SkipInstall, [switch]$Arm64)
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $root
$target = if ($Arm64) { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$release = Join-Path $root "target\$target\release"
$out = Join-Path $root "target\release\windows_release"

function Step($name) { Write-Host "`n==> $name" -ForegroundColor Cyan }
# Windows PowerShell 5.1 does not stop on failing native commands; check exit codes.
function Run {
    & $args[0] @($args | Select-Object -Skip 1)
    if ($LASTEXITCODE -ne 0) { throw "Command failed ($LASTEXITCODE): $($args -join ' ')" }
}

Step "Checking tools"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    # Fall back to the toolchain kept inside the repository, when present.
    $local = Join-Path $root ".tools\cargo\bin\cargo.exe"
    if (-not (Test-Path $local)) { throw "Rust is not installed. Install rustup from https://rustup.rs" }
    $env:CARGO_HOME = Join-Path $root ".tools\cargo"
    $env:RUSTUP_HOME = Join-Path $root ".tools\rustup"
    $env:PATH = "$(Join-Path $root '.tools\cargo\bin');$env:PATH"
}
foreach ($tool in "node", "pnpm") {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "$tool is required (Node.js 24 and pnpm 11)." }
}
Run rustup target add $target
$env:UPDATE_SIGNING_ENABLED = "false"

Step "Preparing vendored sources and web assets"
Run node dev-tools/scripts/prepare-vendor.mjs
if (-not $SkipInstall) { Run pnpm --dir apps/desktop install --frozen-lockfile }
Run pnpm --dir apps/desktop build:quick

Step "Building companion programs (CLI, daemon, browser host)"
Run cargo build --release --locked --target $target -p okytes-cli -p okytes-daemon -p okytes-native-host
Run node packaging/companions.mjs $release $target
Run node packaging/configure.mjs windows .tools-release.json

Step "Building the desktop app, NSIS installer and MSI"
Run pnpm --dir apps/desktop exec tauri build --target $target `
    --config "$root\packaging\windows\tauri.json" --config "$root\.tools-release.json" -- --locked

Step "Building server tools (MCP and cloud)"
Run cargo build --release --locked --target $target -p okytes-mcp -p okytes-cloud

Step "Creating the portable ZIP"
$portable = Join-Path $root "portable"
if (Test-Path $portable) { Remove-Item $portable -Recurse -Force }
Run node packaging/stage.mjs $release portable $target

Step "Collecting into $out"
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item "$release\bundle\nsis\*.exe" $out
Copy-Item "$release\bundle\msi\*.msi" $out
Compress-Archive -Path "$portable\*" -DestinationPath (Join-Path $out "Okytes-portable-$target.zip") -Force
foreach ($exe in "okytes-cli", "okytes-daemon", "okytes-mcp", "okytes-cloud") {
    Copy-Item "$release\$exe.exe" $out
}
Remove-Item $portable -Recurse -Force

Get-ChildItem $out | Format-Table Name, @{ n = "Size (MB)"; e = { [math]::Round($_.Length / 1MB, 1) } } -AutoSize
Write-Host "Done. Installers are unsigned; Windows SmartScreen will warn when they run." -ForegroundColor Green
