<#
.SYNOPSIS
    Benchmark Script: Prist vs Puro (Flutter Version Managers)
.DESCRIPTION
    Measures and compares:
    1. Installation time (Installer execution)
    2. Version creation / setup speed (First & subsequent SDK environments)
    3. Version switching speed (prist use vs puro use)
    4. Disk space consumption & deduplication efficiency (% MB saved)
.USAGE
    powershell -ExecutionPolicy Bypass -File .\scripts\benchmark_prist_vs_puro.ps1
#>

param (
    [string[]]$VersionsToTest = @("stable", "3.24.3"),
    [string]$BenchmarkDir = "$env:TEMP\prist_vs_puro_benchmark"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# ── Helper Functions ─────────────────────────────────────────────────────────

function Write-Header ($text) {
    Write-Host "`n==================================================" -ForegroundColor Cyan
    Write-Host "  $text" -ForegroundColor Header
    Write-Host "==================================================" -ForegroundColor Cyan
}

function Measure-CommandTime ([scriptblock]$ScriptBlock) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $ScriptBlock
    $sw.Stop()
    return [math]::Round($sw.Elapsed.TotalSeconds, 2)
}

function Get-DirectorySizeMB ([string]$Path) {
    if (-not (Test-Path $Path)) { return 0 }
    $bytes = (Get-ChildItem -Path $Path -Recurse -File -ErrorAction SilentlyContinue | 
              Measure-Object -Property Length -Sum).Sum
    return [math]::Round(($bytes / 1MB), 2)
}

# ── 1. Environment Preparation ──────────────────────────────────────────────

Write-Header "PRIST VS PURO BENCHMARK SUITE"
Write-Host "Target versions to test: $($VersionsToTest -join ', ')" -ForegroundColor Yellow
Write-Host "Benchmark sandbox directory: $BenchmarkDir`n"

$pristHome = "$env:LOCALAPPDATA\prist"
$puroHome  = if ($env:PURO_ROOT) { $env:PURO_ROOT } else { "$env:USERPROFILE\.puro" }

# ── 2. Prist Benchmark ───────────────────────────────────────────────────────

Write-Header "Testing Prist (https://prist.dev)"

Write-Host "→ Measuring Prist Installation Time..." -ForegroundColor Gray
$pristInstallTime = Measure-CommandTime {
    Invoke-WebRequest -Uri "https://prist.dev/install.ps1" -OutFile "$env:TEMP\install_prist.ps1"
    & "$env:TEMP\install_prist.ps1" -Force | Out-Null
}
Write-Host "   Prist Install Time: $pristInstallTime s" -ForegroundColor Green

$pristCreateTimes = @{}
$pristUseTimes = @{}

foreach ($ver in $VersionsToTest) {
    $envName = "bench_prist_$($ver.Replace('.', '_'))"
    
    Write-Host "→ Creating Prist Environment '$envName' ($ver)..." -ForegroundColor Gray
    $createTime = Measure-CommandTime {
        prist create $envName $ver | Out-Null
    }
    $pristCreateTimes[$ver] = $createTime
    Write-Host "   Time created ($ver): $createTime s" -ForegroundColor Green

    Write-Host "→ Switching to Prist Environment '$envName'..." -ForegroundColor Gray
    $useTime = Measure-CommandTime {
        prist use $envName | Out-Null
    }
    $pristUseTimes[$ver] = $useTime
    Write-Host "   Time switched ($ver): $useTime s" -ForegroundColor Green
}

$pristDiskSizeMB = Get-DirectorySizeMB $pristHome

# ── 3. Puro Benchmark ────────────────────────────────────────────────────────

Write-Header "Testing Puro (https://puro.dev)"

Write-Host "→ Measuring Puro Installation Time..." -ForegroundColor Gray
$puroInstallTime = Measure-CommandTime {
    Invoke-WebRequest -Uri "https://puro.dev/install.ps1" -OutFile "$env:TEMP\install_puro.ps1"
    & "$env:TEMP\install_puro.ps1" -Force | Out-Null
}
Write-Host "   Puro Install Time: $puroInstallTime s" -ForegroundColor Green

$puroCreateTimes = @{}
$puroUseTimes = @{}

foreach ($ver in $VersionsToTest) {
    $envName = "bench_puro_$($ver.Replace('.', '_'))"
    
    Write-Host "→ Creating Puro Environment '$envName' ($ver)..." -ForegroundColor Gray
    $createTime = Measure-CommandTime {
        puro create $envName $ver | Out-Null
    }
    $puroCreateTimes[$ver] = $createTime
    Write-Host "   Time created ($ver): $createTime s" -ForegroundColor Green

    Write-Host "→ Switching to Puro Environment '$envName'..." -ForegroundColor Gray
    $useTime = Measure-CommandTime {
        puro use $envName | Out-Null
    }
    $puroUseTimes[$ver] = $useTime
    Write-Host "   Time switched ($ver): $useTime s" -ForegroundColor Green
}

$puroDiskSizeMB = Get-DirectorySizeMB $puroHome

# ── 4. Standard Clone Comparison (Baseline Math) ────────────────────────────

# Standard Flutter SDK clone takes ~3,480 MB (3.48 GB) per environment without deduplication
$standardCloneSizeMB = $VersionsToTest.Count * 3480
$pristSavingsPercent = [math]::Round(((1 - ($pristDiskSizeMB / $standardCloneSizeMB)) * 100), 1)
$puroSavingsPercent  = [math]::Round(((1 - ($puroDiskSizeMB / $standardCloneSizeMB)) * 100), 1)

# ── 5. Benchmark Report Summary ──────────────────────────────────────────────

Write-Header "FINAL BENCHMARK COMPARISON REPORT"

$report = [PSCustomObject]@{
    "Metric"                      = "Installer Speed (Seconds)"
    "Prist"                       = "$pristInstallTime s"
    "Puro"                        = "$puroInstallTime s"
    "Winner"                      = if ($pristInstallTime -le $puroInstallTime) { "Prist 🏆" } else { "Puro 🏆" }
}

$reportTable = @($report)

foreach ($ver in $VersionsToTest) {
    $pTime = $pristCreateTimes[$ver]
    $puTime = $puroCreateTimes[$ver]
    $reportTable += [PSCustomObject]@{
        "Metric"                  = "Create Env ($ver)"
        "Prist"                   = "$pTime s"
        "Puro"                    = "$puTime s"
        "Winner"                  = if ($pTime -le $puTime) { "Prist 🏆" } else { "Puro 🏆" }
    }
}

foreach ($ver in $VersionsToTest) {
    $pTime = $pristUseTimes[$ver]
    $puTime = $puroUseTimes[$ver]
    $reportTable += [PSCustomObject]@{
        "Metric"                  = "Switch Env ($ver)"
        "Prist"                   = "$pTime s"
        "Puro"                    = "$puTime s"
        "Winner"                  = if ($pTime -le $puTime) { "Prist 🏆" } else { "Puro 🏆" }
    }
}

$reportTable += [PSCustomObject]@{
    "Metric"                      = "Disk Footprint ($($VersionsToTest.Count) envs)"
    "Prist"                       = "$pristDiskSizeMB MB"
    "Puro"                        = "$puroDiskSizeMB MB"
    "Winner"                      = if ($pristDiskSizeMB -le $puroDiskSizeMB) { "Prist 🏆" } else { "Puro 🏆" }
}

$reportTable += [PSCustomObject]@{
    "Metric"                      = "Disk Savings vs Raw Clones"
    "Prist"                       = "$pristSavingsPercent %"
    "Puro"                        = "$puroSavingsPercent %"
    "Winner"                      = if ($pristSavingsPercent -ge $puroSavingsPercent) { "Prist 🏆" } else { "Puro 🏆" }
}

$reportTable | Format-Table -AutoSize

Write-Host "Benchmark completed successfully." -ForegroundColor Cyan
