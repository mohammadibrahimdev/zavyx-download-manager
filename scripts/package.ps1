# Build every Zavyx DM release file into .\dist\
#
#   ZavyxDM-Setup-<ver>.exe        installer for normal users (needs Inno Setup's iscc)
#   Zavyx DM-<ver>-portable.zip     no-install version
#   ZavyxDM-extension-<ver>.zip    browser extension (for Chrome Web Store / Edge Add-ons)
#
# Usage (from the repo root):
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#
# yt-dlp.exe and ffmpeg.exe are bundled from .\bin\. If they are missing they
# are downloaded from their official release pages (skip with -NoTools).

[CmdletBinding()]
param(
    [switch]$NoTools,
    [switch]$NoInstaller
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "=== Packaging Zavyx DM $version ==="

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$dist = Join-Path $root "dist"
$staging = Join-Path $dist "staging"
if (Test-Path $staging) { Remove-Item -Recurse -Force $staging }
New-Item -ItemType Directory -Force -Path $staging | Out-Null

# App binaries.
foreach ($exe in "ZavyxDM.exe", "zavyx-dm-host.exe", "zavyx-dm-cli.exe") {
    Copy-Item "target\release\$exe" $staging
}

# Third-party tools.
$bin = Join-Path $root "bin"
New-Item -ItemType Directory -Force -Path $bin | Out-Null
if (-not $NoTools) {
    if (-not (Test-Path "$bin\yt-dlp.exe")) {
        Write-Host "Downloading yt-dlp..."
        Invoke-WebRequest "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe" -OutFile "$bin\yt-dlp.exe"
    }
    if (-not (Test-Path "$bin\ffmpeg.exe")) {
        Write-Host "Downloading ffmpeg..."
        $zip = Join-Path $env:TEMP "ffmpeg-essentials.zip"
        Invoke-WebRequest "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" -OutFile $zip
        $tmp = Join-Path $env:TEMP "ffmpeg-essentials"
        if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
        Expand-Archive $zip $tmp
        Copy-Item (Get-ChildItem $tmp -Recurse -Filter ffmpeg.exe | Select-Object -First 1).FullName "$bin\ffmpeg.exe"
    }
}
foreach ($tool in "yt-dlp.exe", "ffmpeg.exe") {
    if (Test-Path "$bin\$tool") { Copy-Item "$bin\$tool" $staging }
}

# Browser extension, licences, and the helper for portable users.
Copy-Item -Recurse extension "$staging\extension"
Copy-Item LICENSE, THIRD-PARTY-NOTICES.md $staging
Copy-Item installer\register-nm-host.ps1, installer\unregister-nm-host.bat $staging
Set-Content -Encoding ASCII "$staging\Connect browsers.bat" @'
@echo off
REM Connects Chrome, Edge, Brave, Vivaldi, Opera and Firefox to this copy of Zavyx DM.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0register-nm-host.ps1" -NmHostPath "%~dp0zavyx-dm-host.exe" -ManifestDir "%~dp0."
pause
'@

# Portable ZIP and extension ZIP.
$portable = Join-Path $dist "ZavyxDM-$version-portable.zip"
$extZip = Join-Path $dist "ZavyxDM-extension-$version.zip"
Remove-Item $portable, $extZip -ErrorAction SilentlyContinue
Compress-Archive -Path "$staging\*" -DestinationPath $portable
Compress-Archive -Path "extension\*" -DestinationPath $extZip
Write-Host "Wrote $portable"
Write-Host "Wrote $extZip"

# Installer (the portable-only helpers aren't needed there).
if (-not $NoInstaller) {
    Remove-Item "$staging\Connect browsers.bat", "$staging\register-nm-host.ps1", "$staging\unregister-nm-host.bat"
    $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
    if (-not $iscc) {
        $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
            Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if ($iscc) {
        & $iscc "/DAppVersion=$version" installer\zavyx-dm.iss
        if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }
        Write-Host "Wrote dist\ZavyxDM-Setup-$version.exe"
    } else {
        Write-Warning "Inno Setup (iscc) not found; skipped the installer. GitHub Actions builds it on release."
    }
}
