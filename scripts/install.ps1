# Prist installer for Windows.
# Usage:
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/SnaidenP/prist/main/scripts/install.ps1 | iex"
#
# Or download and run locally:
#   .\install.ps1
#
# Installs prist to %LOCALAPPDATA%\prist\bin and adds it to the user PATH.
# No admin/UAC required.

#Requires -Version 5.1

[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\prist\bin",
    [string]$Version    = "latest",
    [switch]$Force,
    [switch]$Silent
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$RepoOwner = "SnaidenP"
$RepoName  = "prist"
$BinName   = "prist.exe"

function Write-Step  { param([string]$msg) Write-Host "  ==> $msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$msg) Write-Host "      $msg" -ForegroundColor Green }
function Write-Err  { param([string]$msg) Write-Host "      ERROR: $msg" -ForegroundColor Red }
function Write-Warn { param([string]$msg) Write-Host "      WARN: $msg" -ForegroundColor Yellow }

# ── 1. Detect architecture ──────────────────────────────────────────────────

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64"   { "x86_64-pc-windows-msvc" }
    "ARM64"   { "aarch64-pc-windows-msvc" }
    default   {
        Write-Err "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
        Write-Err "Prist supports x64 and arm64 Windows."
        exit 1
    }
}
Write-Step "Detected architecture: $arch"

if (Get-Command git -ErrorAction SilentlyContinue) {
    Write-Ok "Git detected"
} else {
    Write-Warn "Git is not installed or not in PATH."
    Write-Warn "Prist uses Git to clone and deduplicate Flutter SDKs."
    Write-Warn "You can install Git via: winget install Git.Git"
}

# ── 2. Resolve version + download URL ─────────────────────────────────────────

Write-Step "Resolving version ($Version)..."
$apiBase = "https://api.github.com/repos/$RepoOwner/$RepoName/releases"

if ($Version -eq "latest") {
    $releaseUri = "$apiBase/latest"
} else {
    $releaseUri = "$apiBase/tags/$Version"
}

try {
    $release = Invoke-RestMethod -Uri $releaseUri -Headers @{ "User-Agent" = "prist-installer" } -ErrorAction Stop
} catch {
    Write-Err "Could not fetch release info from GitHub: $($_.Exception.Message)"
    Write-Err "Check that the repo $RepoOwner/$RepoName has published releases."
    exit 1
}

$tag   = $release.tag_name

# Try tar.gz first (self_update format), fall back to zip.
$assetTgz = "$BinName-$arch.tar.gz"
$assetZip = "$BinName-$arch.zip"
$asset = $release.assets | Where-Object { $_.name -eq $assetTgz } | Select-Object -First 1
$assetName = $assetTgz
if (-not $asset) {
    $asset = $release.assets | Where-Object { $_.name -eq $assetZip } | Select-Object -First 1
    $assetName = $assetZip
}

if (-not $asset) {
    Write-Err "No asset named '$assetTgz' or '$assetZip' found in release $tag."
    Write-Err "Available assets:"
    $release.assets | ForEach-Object { Write-Host "        - $($_.name)" }
    exit 1
}

$downloadUrl = $asset.browser_download_url
Write-Ok "Release $tag → $assetName"

# ── 3. Check for existing installation ────────────────────────────────────────

$destExe = Join-Path $InstallDir $BinName
if (Test-Path $destExe) {
    $existing = & $destExe --version 2>$null
    if ($existing -and -not $Force) {
        if (-not $Silent) {
            Write-Warn "Prist is already installed at $InstallDir ($existing)"
            $choice = Read-Host "      Reinstall/upgrade? [y/N]"
            if ($choice -notmatch '^[yY]') {
                Write-Host "      Aborted."
                exit 0
            }
        }
    }
    Write-Step "Removing previous installation..."
    Remove-Item $destExe -Force -ErrorAction SilentlyContinue
}

# ── 4. Download ──────────────────────────────────────────────────────────────

Write-Step "Downloading $downloadUrl..."
$randomId = [System.IO.Path]::GetRandomFileName()
$zipPath = Join-Path $env:TEMP "prist-$tag-$arch-$randomId.zip"

try {
    $wc = New-Object System.Net.WebClient
    $wc.Headers.Add("User-Agent", "prist-installer")
    $wc.DownloadFile($downloadUrl, $zipPath)
} catch {
    Write-Err "Download failed: $($_.Exception.Message)"
    exit 1
}
Write-Ok "Downloaded $([math]::Round((Get-Item $zipPath).Length / 1MB, 1)) MB"

# ── 5. Verify checksum (if .sha256 sidecar exists) ───────────────────────────

$shaAssetName = "$assetName.sha256"
$shaAsset = $release.assets | Where-Object { $_.name -eq $shaAssetName } | Select-Object -First 1
if ($shaAsset) {
    Write-Step "Verifying checksum..."
    try {
        $shaResp = Invoke-WebRequest -Uri $shaAsset.browser_download_url -UseBasicParsing
        $expectedRaw = $shaResp.Content
        # GitHub serves .sha256 as application/octet-stream, so .Content may be a byte[].
        if ($expectedRaw -is [byte[]]) {
            $expectedRaw = [System.Text.Encoding]::UTF8.GetString($expectedRaw)
        }
        $expected = ($expectedRaw -split '\s+')[0].Trim().ToLower()
        $actual = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) {
            Write-Err "Checksum mismatch!"
            Write-Err "  expected: $expected"
            Write-Err "  actual:   $actual"
            Remove-Item $zipPath -Force
            exit 1
        }
        Write-Ok "Checksum verified"
    } catch {
        Write-Warn "Could not verify checksum: $($_.Exception.Message)"
    }
}

# ── 6. Extract ───────────────────────────────────────────────────────────────

Write-Step "Extracting to $InstallDir..."
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

try {
    if ($assetName -like "*.tar.gz") {
        # Extract tar.gz using tar (available on Windows 10+)
        tar -xzf $zipPath -C $InstallDir 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "tar extraction failed (exit $LASTEXITCODE)" }
    } else {
        Expand-Archive -Path $zipPath -DestinationPath $InstallDir -Force -ErrorAction Stop
    }
} catch {
    Write-Err "Extraction failed: $($_.Exception.Message)"
    exit 1
}

# If the zip has a top-level folder, flatten it.
$nestedExe = Get-ChildItem -Path $InstallDir -Recurse -Filter $BinName | Select-Object -First 1
if ($nestedExe -and $nestedExe.FullName -ne $destExe) {
    Move-Item $nestedExe.FullName $destExe -Force
    # Clean up any empty subdirectories left by a nested structure.
    Get-ChildItem -Path $InstallDir -Directory | Where-Object { -not (Get-ChildItem $_.FullName -Recurse -File) } | Remove-Item -Recurse -Force
}

Remove-Item $zipPath -Force
Write-Ok "Installed $BinName → $destExe"

# ── 7. Add to PATH (user-level, no admin) ─────────────────────────────────────

Write-Step "Adding to user PATH..."
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")

if ($userPath -split ';' -contains $InstallDir) {
    Write-Ok "PATH already contains $InstallDir"
} else {
    $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    # Also update the current session so `prist` works immediately.
    $env:Path = "$env:Path;$InstallDir"
    Write-Ok "Added $InstallDir to PATH (user-level)"
}

# ── 8. Verify ────────────────────────────────────────────────────────────────

Write-Step "Verifying installation..."
$installed = & $destExe --version 2>$null
if ($installed) {
    Write-Ok "Prist $installed installed successfully."
} else {
    Write-Warn "prist.exe exists but did not respond to --version."
}

# ── 9. Next steps ────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "  Next steps:" -ForegroundColor White
Write-Host "    Open a NEW terminal (to pick up the PATH change), then:" -ForegroundColor Gray
Write-Host "      prist create my_app stable" -ForegroundColor Yellow
Write-Host "      prist use my_app" -ForegroundColor Yellow
Write-Host "      prist flutter run" -ForegroundColor Yellow
Write-Host ""
if (-not $Silent) {
    Write-Host "  Done. Press any key to exit..." -ForegroundColor DarkGray
    $null = $Host.UI.RawUI.ReadKey("NoEcho,IncludeKeyDown")
}
