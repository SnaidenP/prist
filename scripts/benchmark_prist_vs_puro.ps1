<#
.SYNOPSIS
    100% Fair Clean-Slate Benchmark: Prist vs Puro
.DESCRIPTION
    Wipes both Prist and Puro data completely to run a 100% scratch benchmark.
    Measures:
    1. Installation time (Official installers)
    2. Initial Environment creation speed (Flutter stable)
    3. Secondary Environment creation speed (Flutter 3.24.3)
    4. Version switching speed (prist use vs puro use)
    5. Real disk space footprint (%LOCALAPPDATA%\prist vs %USERPROFILE%\.puro)
.USAGE
    powershell -ExecutionPolicy Bypass -File .\scripts\benchmark_prist_vs_puro.ps1
#>

param (
    [string[]]$VersionsToTest = @("stable", "3.24.3")
)

$ErrorActionPreference = "Continue"
$ProgressPreference = "SilentlyContinue"

function Write-Header ($text) {
    Write-Host "`n==================================================" -ForegroundColor Cyan
    Write-Host "  $text" -ForegroundColor Yellow
    Write-Host "==================================================" -ForegroundColor Cyan
}

function Measure-CommandTime ([scriptblock]$ScriptBlock) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $ScriptBlock 2>&1 | Out-Null
    $sw.Stop()
    return [math]::Round($sw.Elapsed.TotalSeconds, 2)
}

function Get-DirectorySizeMB ([string]$Path) {
    if (-not (Test-Path $Path)) { return 0 }
    $bytes = (Get-ChildItem -Path $Path -Recurse -File -ErrorAction SilentlyContinue | 
              Measure-Object -Property Length -Sum).Sum
    return [math]::Round(($bytes / 1MB), 2)
}

Write-Header "100% CLEAN-SLATE BENCHMARK: PRIST VS PURO"

$pristHome = "$env:LOCALAPPDATA\prist"
$puroHome  = "$env:USERPROFILE\.puro"

# ── 1. Clean Slate Cleanup ──────────────────────────────────────────────────
Write-Host "→ Cleaning existing data directories..." -ForegroundColor Gray
if (Test-Path $pristHome) { Remove-Item -Recurse -Force $pristHome -ErrorAction SilentlyContinue }
if (Test-Path $puroHome)  { Remove-Item -Recurse -Force $puroHome  -ErrorAction SilentlyContinue }

# ── 2. Prist Benchmark (From Scratch) ───────────────────────────────────────
Write-Header "1. Testing Prist (From Scratch)"

Write-Host "→ Installing Prist..." -ForegroundColor Gray
$pristInstallTime = Measure-CommandTime {
    $wc = New-Object System.Net.WebClient
    $wc.DownloadFile("https://prist.dev/install.ps1", "$env:TEMP\install_prist.ps1")
    & "$env:TEMP\install_prist.ps1" -Force -Silent | Out-Null
}
Write-Host "   Prist Installation: $pristInstallTime s" -ForegroundColor Green

$pristExe = "$pristHome\bin\prist.exe"
$pristCreateTimes = @{}
$pristUseTimes = @{}

foreach ($ver in $VersionsToTest) {
    $envName = "prist_env_$($ver.Replace('.', '_'))"
    
    Write-Host "→ Prist: Creating '$envName' ($ver)..." -ForegroundColor Gray
    $createTime = Measure-CommandTime {
        & $pristExe create $envName $ver
    }
    $pristCreateTimes[$ver] = $createTime
    Write-Host "   Prist Created ($ver): $createTime s" -ForegroundColor Green

    Write-Host "→ Prist: Switching to '$envName'..." -ForegroundColor Gray
    $useTime = Measure-CommandTime {
        & $pristExe use $envName
    }
    $pristUseTimes[$ver] = $useTime
    Write-Host "   Prist Switched ($ver): $useTime s" -ForegroundColor Green
}

$pristDiskSizeMB = Get-DirectorySizeMB $pristHome

# ── 3. Puro Benchmark (From Scratch) ────────────────────────────────────────
Write-Header "2. Testing Puro (From Scratch)"

Write-Host "→ Installing Puro..." -ForegroundColor Gray
$puroInstallTime = Measure-CommandTime {
    $wc = New-Object System.Net.WebClient
    $wc.DownloadFile("https://puro.dev/builds/1.5.0/windows-x64/puro.exe", "$env:TEMP\puro.exe")
    & "$env:TEMP\puro.exe" install-puro --promote --y | Out-Null
}
Write-Host "   Puro Installation: $puroInstallTime s" -ForegroundColor Green

$puroExe = if (Test-Path "$puroHome\bin\puro.exe") { "$puroHome\bin\puro.exe" } else { "$env:TEMP\puro.exe" }
$puroCreateTimes = @{}
$puroUseTimes = @{}

foreach ($ver in $VersionsToTest) {
    $envName = "puro_env_$($ver.Replace('.', '_'))"
    
    Write-Host "→ Puro: Creating '$envName' ($ver)..." -ForegroundColor Gray
    $createTime = Measure-CommandTime {
        & $puroExe create $envName $ver --force -y
    }
    $puroCreateTimes[$ver] = $createTime
    Write-Host "   Puro Created ($ver): $createTime s" -ForegroundColor Green

    Write-Host "→ Puro: Switching to '$envName'..." -ForegroundColor Gray
    $useTime = Measure-CommandTime {
        & $puroExe use $envName
    }
    $puroUseTimes[$ver] = $useTime
    Write-Host "   Puro Switched ($ver): $useTime s" -ForegroundColor Green
}

$puroDiskSizeMB = Get-DirectorySizeMB $puroHome

# ── 4. Final Comparison Report Table ───────────────────────────────────────
Write-Header "FINAL FAIR BENCHMARK REPORT"

$reportTable = @()

$reportTable += [PSCustomObject]@{
    "Metric / Operation"         = "1. Installer Speed"
    "Prist (Rust)"              = "$pristInstallTime s"
    "Puro (Dart)"               = "$puroInstallTime s"
    "Winner"                    = if ($pristInstallTime -le $puroInstallTime) { "Prist 🏆" } else { "Puro 🏆" }
}

foreach ($ver in $VersionsToTest) {
    $pTime  = $pristCreateTimes[$ver]
    $puTime = $puroCreateTimes[$ver]
    $reportTable += [PSCustomObject]@{
        "Metric / Operation"     = "2. Create Env ($ver)"
        "Prist (Rust)"          = "$pTime s"
        "Puro (Dart)"           = "$puTime s"
        "Winner"                = if ($pTime -le $puTime) { "Prist 🏆" } else { "Puro 🏆" }
    }
}

foreach ($ver in $VersionsToTest) {
    $pTime  = $pristUseTimes[$ver]
    $puTime = $puroUseTimes[$ver]
    $reportTable += [PSCustomObject]@{
        "Metric / Operation"     = "3. Switch Env ($ver)"
        "Prist (Rust)"          = "$pTime s"
        "Puro (Dart)"           = "$puTime s"
        "Winner"                = if ($pTime -le $puTime) { "Prist 🏆" } else { "Puro 🏆" }
    }
}

$reportTable += [PSCustomObject]@{
    "Metric / Operation"         = "4. Total Disk Footprint ($($VersionsToTest.Count) envs)"
    "Prist (Rust)"              = "$pristDiskSizeMB MB"
    "Puro (Dart)"               = "$puroDiskSizeMB MB"
    "Winner"                    = if ($pristDiskSizeMB -le $puroDiskSizeMB) { "Prist 🏆" } else { "Puro 🏆" }
}

$reportTable | Format-Table -AutoSize

Write-Host "Benchmark from scratch completed." -ForegroundColor Cyan
