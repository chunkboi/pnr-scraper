<#
.SYNOPSIS
    Bootstrap the build environment for scraper_rs.

.DESCRIPTION
    1. Clones the microsoft/vcpkg repository into ./vcpkg_tools (if absent).
    2. Bootstraps vcpkg (compiles vcpkg.exe).
    3. Installs tesseract:x64-windows-static-md (and all its dependencies).
    4. Runs "vcpkg integrate install" so the vcpkg-rs crate can locate the
       installation automatically via %LOCALAPPDATA%\vcpkg\vcpkg.user.targets.

    After this script completes, run:
        cargo build --release

.PARAMETER VcpkgDir
    Directory to install vcpkg into.  Defaults to "vcpkg_tools" (repo root).

.PARAMETER SkipIntegrate
    Skip "vcpkg integrate install".  Useful in CI or non-admin environments
    where the system-wide integration is not needed (VCPKG_ROOT in
    .cargo/config.toml already points here).

.EXAMPLE
    .\setup.ps1
    .\setup.ps1 -VcpkgDir C:\vcpkg
    .\setup.ps1 -SkipIntegrate
#>

[CmdletBinding()]
param(
    [string]$VcpkgDir    = "vcpkg_tools",
    [switch]$SkipIntegrate
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# ── Helpers ───────────────────────────────────────────────────────────────────

function Write-Step([string]$msg) {
    Write-Host "`n==> $msg" -ForegroundColor Cyan
}

function Write-Ok([string]$msg) {
    Write-Host "    [OK] $msg" -ForegroundColor Green
}

function Write-Warn([string]$msg) {
    Write-Host "    [!!] $msg" -ForegroundColor Yellow
}

function Assert-Command([string]$name) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        Write-Host "ERROR: '$name' was not found in PATH." -ForegroundColor Red
        Write-Host "       Please install it and re-run this script." -ForegroundColor Red
        exit 1
    }
}

# ── Pre-flight checks ─────────────────────────────────────────────────────────

Write-Step "Checking prerequisites"

Assert-Command "git"
Assert-Command "cargo"

# MSVC cl.exe is required by vcpkg to compile native libraries
$vsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $vsWhere) {
    $clPath = & $vsWhere -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                         -find "VC\Tools\MSVC\**\bin\Hostx64\x64\cl.exe" 2>$null |
              Select-Object -First 1
    if ($clPath) {
        Write-Ok "MSVC cl.exe found: $clPath"
    } else {
        Write-Warn "MSVC C++ toolchain not found via vswhere."
        Write-Warn "Install 'Desktop development with C++' in Visual Studio Installer."
    }
} else {
    Write-Warn "vswhere not found — skipping MSVC check (ensure cl.exe is in PATH)."
}

# ── Clone vcpkg ───────────────────────────────────────────────────────────────

$vcpkgExe = Join-Path $VcpkgDir "vcpkg.exe"

if (Test-Path $vcpkgExe) {
    Write-Step "vcpkg already present at '$VcpkgDir' — skipping clone"
    Write-Ok $vcpkgExe
} else {
    if (Test-Path $VcpkgDir) {
        Write-Step "Directory '$VcpkgDir' exists but vcpkg.exe is missing — bootstrapping"
    } else {
        Write-Step "Cloning microsoft/vcpkg into '$VcpkgDir'"
        git clone https://github.com/microsoft/vcpkg.git $VcpkgDir
        Write-Ok "Clone complete"
    }

    Write-Step "Bootstrapping vcpkg (compiling vcpkg.exe — this takes ~1 minute)"
    $bootstrap = Join-Path $VcpkgDir "bootstrap-vcpkg.bat"
    if (-not (Test-Path $bootstrap)) {
        Write-Host "ERROR: bootstrap-vcpkg.bat not found in '$VcpkgDir'." -ForegroundColor Red
        exit 1
    }
    & cmd.exe /c "`"$bootstrap`" -disableMetrics"
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: Bootstrap failed (exit code $LASTEXITCODE)." -ForegroundColor Red
        exit 1
    }
    Write-Ok "vcpkg.exe ready"
}

# ── Install packages ──────────────────────────────────────────────────────────

Write-Step "Installing tesseract:x64-windows-static-md"
Write-Host "    (This compiles Tesseract, Leptonica, libjpeg, libpng, libtiff, zlib, webp, ...)"
Write-Host "    First run takes 10–25 minutes depending on your machine.`n"

$packages = @(
    "tesseract:x64-windows-static-md"
)

foreach ($pkg in $packages) {
    Write-Host "    Installing $pkg ..." -ForegroundColor DarkCyan
    & $vcpkgExe install $pkg
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: 'vcpkg install $pkg' failed (exit code $LASTEXITCODE)." -ForegroundColor Red
        exit 1
    }
    Write-Ok "$pkg installed"
}

# ── System integration ────────────────────────────────────────────────────────

if ($SkipIntegrate) {
    Write-Step "Skipping vcpkg integrate install (-SkipIntegrate was set)"
    Write-Warn "VCPKG_ROOT in .cargo/config.toml will be used to locate vcpkg."
} else {
    Write-Step "Running 'vcpkg integrate install'"
    & $vcpkgExe integrate install
    if ($LASTEXITCODE -ne 0) {
        Write-Warn "'vcpkg integrate install' failed — you may need to run it as Administrator."
        Write-Warn "The build will still work via VCPKG_ROOT in .cargo/config.toml."
    } else {
        Write-Ok "Integration complete"
    }
}

# ── Done ──────────────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Green
Write-Host "  Setup complete!  Build the release binary with:" -ForegroundColor Green
Write-Host ""
Write-Host "      cargo build --release" -ForegroundColor White
Write-Host ""
Write-Host "  The binary will be at:  target\release\scraper_rs.exe" -ForegroundColor DarkGray
Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Green
Write-Host ""
