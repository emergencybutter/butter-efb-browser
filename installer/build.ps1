# Builds dist\EFB-Browser-Setup-<version>.msi and .exe:
#   1. the EFB app (npm run build) and its Community package (node efb\pack.js)
#   2. the companion, which embeds that package (cargo build --release)
#   3. the installers (Inno Setup 6, then WiX 7)
#
# Usage: powershell -ExecutionPolicy Bypass -File installer\build.ps1 [-SkipEfb]
param([switch]$SkipEfb)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Run($exe, [string[]]$argv) {
    & $exe @argv
    if ($LASTEXITCODE -ne 0) { throw "$exe $($argv -join ' ') failed ($LASTEXITCODE)" }
}

if (-not $SkipEfb) {
    Step 'EFB app'
    Push-Location "$root\efb\PackageSources\BrowserApp"
    try {
        if (-not (Test-Path node_modules)) { Run npm @('ci') }
        Run npm @('run', 'build')
    } finally { Pop-Location }
    Run node @("$root\efb\pack.js")
}

Step 'Companion'
Run cargo @('build', '--release', '--manifest-path', "$root\companion\Cargo.toml")

Step 'Installer'
$version = (Select-String -Path "$root\companion\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$iscc = @(
    (Get-Command iscc -ErrorAction SilentlyContinue).Source,
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found: winget install JRSoftware.InnoSetup' }
Run $iscc @("/Q", "/DAppVersion=$version", "$PSScriptRoot\EFBBrowser.iss")

Step 'MSI'
# WiX 7 (winget install WiXToolset.WiXCLI) needs its OSMF EULA accepted once: wix eula accept wix7
$wix = @(
    (Get-Command wix -ErrorAction SilentlyContinue).Source,
    "$env:ProgramFiles\WiX Toolset v7.0\bin\wix.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if ($wix) {
    Run $wix @('extension', 'add', '-g', 'WixToolset.Util.wixext/7.0.1', 'WixToolset.UI.wixext/7.0.1')
    Run $wix @('build', '-arch', 'x64', '-ext', 'WixToolset.Util.wixext', '-ext', 'WixToolset.UI.wixext', '-d', "Version=$version",
        '-o', "$root\dist\EFB-Browser-Setup-$version.msi", "$PSScriptRoot\EFBBrowser.wxs")
} else {
    Write-Warning 'WiX 7 not found, MSI skipped: winget install WiXToolset.WiXCLI'
}
Write-Host "`nInstallers in $root\dist:" -ForegroundColor Green
Get-ChildItem "$root\dist\EFB-Browser-Setup-$version*" -Include *.exe, *.msi | ForEach-Object { Write-Host "  $($_.Name)" -ForegroundColor Green }
