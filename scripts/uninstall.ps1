# Prist uninstaller for Windows.
# Usage:
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/SnaidenP/prist/main/scripts/uninstall.ps1 | iex"
#
# Or download and run locally:
#   .\uninstall.ps1
#
# Removes prist.exe from %LOCALAPPDATA%\prist\bin and removes it from the user PATH.
# Does NOT remove installed Flutter environments (~/.prist or %LOCALAPPDATA%\prist).

#Requires -Version 5.1

[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\prist\bin",
    [switch]$Purge,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$BinName = "prist.exe"

function Write-Step { param([string]$msg) Write-Host "  ==> $msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$msg) Write-Host "      $msg" -ForegroundColor Green }
function Write-Err  { param([string]$msg) Write-Host "      ERROR: $msg" -ForegroundColor Red }
function Write-Warn { param([string]$msg) Write-Host "      WARN: $msg" -ForegroundColor Yellow }

# ── 1. Confirm ───────────────────────────────────────────────────────────────

$destExe = Join-Path $InstallDir $BinName

if (-not (Test-Path $destExe)) {
    Write-Warn "prist.exe not found at $destExe — nothing to uninstall."
    exit 0
}

if (-not $Force) {
    $msg = if ($Purge) {
        "This will remove prist.exe AND all installed Flutter environments. Continue? [y/N]"
    } else {
        "This will remove prist.exe from $InstallDir. Continue? [y/N]"
    }
    $choice = Read-Host $msg
    if ($choice -notmatch '^[yY]') {
        Write-Host "      Aborted."
        exit 0
    }
}

# ── 2. Remove binary ─────────────────────────────────────────────────────────

Write-Step "Removing $destExe..."
Remove-Item $destExe -Force -ErrorAction SilentlyContinue
Write-Ok "Removed prist.exe"

# Remove the bin directory if it's now empty.
$remaining = Get-ChildItem -Path $InstallDir -ErrorAction SilentlyContinue
if (-not $remaining) {
    Remove-Item $InstallDir -Force -ErrorAction SilentlyContinue
    Write-Ok "Removed empty directory $InstallDir"
}

# ── 3. Remove from PATH ──────────────────────────────────────────────────────

Write-Step "Removing from user PATH..."
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath) {
    $parts = $userPath -split ';' | Where-Object { $_ -ne $InstallDir -and $_ -ne '' }
    $newPath = $parts -join ';'
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Ok "Removed $InstallDir from PATH"
}

# ── 4. Optionally purge all environments ─────────────────────────────────────

if ($Purge) {
    $pristHome = "$env:LOCALAPPDATA\prist"
    if (Test-Path $pristHome) {
        Write-Step "Purging Prist home at $pristHome..."
        Remove-Item $pristHome -Recurse -Force
        Write-Ok "Purged $pristHome"
    }
}

# ── 5. Done ──────────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "  Prist uninstalled successfully." -ForegroundColor Green
if (-not $Purge) {
    Write-Host "  Flutter environments under ~\AppData\Local\prist were preserved." -ForegroundColor DarkGray
    Write-Host "  Use -Purge to remove them as well." -ForegroundColor DarkGray
}
Write-Host ""
