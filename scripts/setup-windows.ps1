# Mirr — Windows toolchain bootstrap + first run
# Installs Rust, MSVC C++ Build Tools, and the Tauri CLI, then launches `tauri dev`.
# Heavy downloads (Build Tools is multiple GB). Run from an elevated PowerShell.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
Write-Host "== Mirr setup ==" -ForegroundColor Cyan

function Have($cmd) { return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

# 1) Rust
if (Have rustc) {
    Write-Host "Rust already installed: $(rustc --version)" -ForegroundColor Green
} else {
    Write-Host "Installing Rust via winget..." -ForegroundColor Yellow
    winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
    $env:Path += ";$env:USERPROFILE\.cargo\bin"
}

# 2) MSVC C++ Build Tools (required to link Rust on Windows)
if (Have cl) {
    Write-Host "MSVC build tools present." -ForegroundColor Green
} else {
    Write-Host "Installing Visual Studio C++ Build Tools (large download)..." -ForegroundColor Yellow
    winget install --id Microsoft.VisualStudio.2022.BuildTools -e `
        --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" `
        --accept-source-agreements --accept-package-agreements
    Write-Host "You may need to reopen PowerShell so the linker is on PATH." -ForegroundColor Yellow
}

# 3) UxPlay — the AirPlay engine Mirr supervises
if (Test-Path "C:\Program Files (x86)\uxplay-windows\_internal\bin\uxplay.exe") {
    Write-Host "UxPlay present." -ForegroundColor Green
} else {
    Write-Host "Installing UxPlay via winget..." -ForegroundColor Yellow
    winget install --id leapbtw.uxplay -e --accept-source-agreements --accept-package-agreements
}

# 4) Firewall: allow the AirPlay engine (mirroring uses inbound TCP+UDP)
$uxexe = "C:\Program Files (x86)\uxplay-windows\_internal\bin\uxplay.exe"
if ((Test-Path $uxexe) -and -not (Get-NetFirewallRule -DisplayName "Mirr UxPlay TCP" -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -DisplayName "Mirr UxPlay TCP" -Direction Inbound -Program $uxexe -Protocol TCP -Action Allow -Profile Any | Out-Null
    New-NetFirewallRule -DisplayName "Mirr UxPlay UDP" -Direction Inbound -Program $uxexe -Protocol UDP -Action Allow -Profile Any | Out-Null
    Enable-NetFirewallRule -DisplayGroup "mDNS" -ErrorAction SilentlyContinue
    Write-Host "Firewall rules added." -ForegroundColor Green
}

# 5) Tauri CLI (via npm)
Write-Host "Installing Tauri CLI..." -ForegroundColor Yellow
npm install

# 6) Run
Write-Host "Launching 'tauri dev' (first compile takes a few minutes)..." -ForegroundColor Cyan
npm run dev
