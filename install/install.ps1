# Coffee CLI Mechoy Build - Windows Installer / Updater
# Usage:   irm https://raw.githubusercontent.com/Mechoy/Coffee-CLI/main/install/install.ps1 | iex
# License: AGPL-3.0-or-later (https://github.com/Mechoy/Coffee-CLI/blob/main/LICENSE)

$ErrorActionPreference = "Stop"
$productName = "Coffee CLI Mechoy"
$legacyProductName = "Coffee CLI"

Write-Host ""
Write-Host "  $productName Installer" -ForegroundColor Cyan
Write-Host "  --------------------" -ForegroundColor DarkGray

# Mechoy builds read a marker that CI updates only after the matching release
# is published. This avoids GitHub Releases API rate limits and prevents an
# official Coffee CLI package from replacing local functionality.
Write-Host "  Fetching latest version..." -ForegroundColor Gray
$latestVer = $null
$fallbackUrl = $null
try {
    $marker = Invoke-RestMethod "https://raw.githubusercontent.com/Mechoy/Coffee-CLI/main/Web-Home/mechoy-version.json" `
        -Headers @{ "User-Agent" = "CoffeeCLI-Mechoy-Install" } -TimeoutSec 15
    if ($marker.version -match '^(?<version>\d+\.\d+\.\d+)$') {
        $latestVer = $Matches.version
        $expectedAsset = "Coffee.CLI_Mechoy_${latestVer}_Windows_x64-setup.exe"
        $fallbackUrl = "https://github.com/Mechoy/Coffee-CLI/releases/download/mechoy-v${latestVer}/${expectedAsset}"
    }
} catch {}

# Detect currently installed version from Windows registry
$installedVer = $null
$legacyInstalledVer = $null
$regPaths = @(
    "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*",
    "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*",
    "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*"
)
foreach ($path in $regPaths) {
    $entries = @(Get-ItemProperty $path -ErrorAction SilentlyContinue)
    $entry = $entries |
             Where-Object { $_.DisplayName -eq $productName } |
             Select-Object -First 1
    if ($entry) {
        $installedVer = $entry.DisplayVersion
        break
    }
    if (-not $legacyInstalledVer) {
        $legacyEntry = $entries |
                       Where-Object { $_.DisplayName -eq $legacyProductName } |
                       Select-Object -First 1
        if ($legacyEntry) {
            $legacyInstalledVer = $legacyEntry.DisplayVersion
        }
    }
}

# Empty `version` = the Windows build isn't out yet (CI probably still
# running for a just-tagged release). Show an explicit "come back later"
# message and pause so the window doesn't auto-close on the user before
# they read it (some launch flows spawn a fresh PowerShell that closes
# the moment the script returns).
if (-not $latestVer) {
    Write-Host ""
    Write-Host "  No matching Mechoy Build installer is available yet." -ForegroundColor Yellow
    Write-Host "  Check the release status and try again in about 10 minutes." -ForegroundColor Yellow
    Write-Host ""
    if ($installedVer) {
        Write-Host "  Your current v$installedVer stays installed." -ForegroundColor Gray
        Write-Host ""
    }
    Write-Host "  Press any key to close..." -ForegroundColor DarkGray
    # ReadKey reads from the console keyboard buffer directly, so it works
    # even when stdin is consumed by `irm | iex`. The try/catch swallows
    # the case where there is no interactive console (CI / redirected).
    try { [void][System.Console]::ReadKey($true) } catch {}
    exit 0
}

Write-Host "  Latest : v$latestVer" -ForegroundColor Green

if ($installedVer) {
    Write-Host "  Installed: v$installedVer" -ForegroundColor Gray
    $comparison = $null
    try { $comparison = ([version]$installedVer).CompareTo([version]$latestVer) } catch {}
    if ($null -ne $comparison -and $comparison -ge 0) {
        Write-Host ""
        if ($comparison -eq 0) {
            Write-Host "  $productName is already up to date (v$installedVer)." -ForegroundColor Green
        } else {
            Write-Host "  A newer $productName build is already installed (v$installedVer)." -ForegroundColor Green
        }
        Write-Host ""
        exit 0
    }
    Write-Host "  Upgrading $productName v$installedVer -> v$latestVer ..." -ForegroundColor Yellow
} else {
    if ($legacyInstalledVer) {
        Write-Host "  Legacy $legacyProductName v$legacyInstalledVer will be left unchanged." -ForegroundColor Yellow
    }
    Write-Host "  Not installed - performing fresh install..." -ForegroundColor Gray
}

$url = $fallbackUrl
$out = "$env:TEMP\coffee-cli-mechoy-setup.exe"

Write-Host "  Downloading..." -ForegroundColor Gray
# Wrap in try/catch so a transient 404 (CI edge case: version.json says
# ready but GitHub asset not yet consistent) surfaces as a friendly
# message instead of a raw WebException stack.
try {
    Invoke-WebRequest $url -OutFile $out -UseBasicParsing
} catch {
    Write-Host ""
    Write-Host "  Download failed: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "  The Windows installer may still be uploading to GitHub." -ForegroundColor DarkYellow
    Write-Host "  Please wait ~5 minutes and run this command again." -ForegroundColor DarkYellow
    Write-Host ""
    exit 1
}

Write-Host "  Installing..." -ForegroundColor Gray
Start-Process $out -Wait

Write-Host ""
Write-Host "  Done! $productName v$latestVer installed." -ForegroundColor Green
Write-Host "  Launch it from the Start Menu." -ForegroundColor Gray
Write-Host ""
