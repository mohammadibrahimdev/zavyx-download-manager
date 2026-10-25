# Register the Zavyx Native Messaging host manually (no installer).
#
# Run this PowerShell script as the current user (no admin needed) to:
#   1. Place io.github.mohammadibrahimdev.zavyx.json in %LOCALAPPDATA%\Zavyx\
#   2. Patch the `path` field inside it to point at the installed binary
#   3. Register the JSON path in HKCU for Chrome, Edge, Brave, Vivaldi, Opera, Firefox
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File register-nm-host.ps1 -NmHostPath "C:\Program Files\Zavyx\bin\zavyx-host.exe"

[CmdletBinding()]
param(
    [string]$NmHostPath = "C:\Program Files\Zavyx\bin\zavyx-host.exe",
    [string]$ManifestDir = "$env:LOCALAPPDATA\Zavyx"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $NmHostPath)) {
    Write-Error "Native messaging binary not found at: $NmHostPath"
    Write-Error "Build the Rust workspace first:  cargo build --release -p zavyx-cli"
    exit 1
}

# Create manifest dir if needed.
if (-not (Test-Path $ManifestDir)) {
    New-Item -ItemType Directory -Path $ManifestDir -Force | Out-Null
}

# Write the manifest JSON. We hard-code the host name "io.github.mohammadibrahimdev.zavyx".
$manifestPath = Join-Path $ManifestDir "io.github.mohammadibrahimdev.zavyx.json"
$manifest = @{
    name = "io.github.mohammadibrahimdev.zavyx"
    description = "Zavyx download manager native messaging host"
    path = $NmHostPath
    type = "stdio"
    # Chromium requires exact origins (no wildcards). The ID is fixed by the
    # "key" field in extension\manifest.json.
    allowed_origins = @("chrome-extension://gileklmimbdimpngahanhmggcejpiaal/")
} | ConvertTo-Json -Depth 4

# Write UTF-8 without BOM (PowerShell 5.1's -Encoding UTF8 adds one).
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($manifestPath, $manifest, $utf8NoBom)
Write-Host "Wrote manifest: $manifestPath"

# Registry keys for each Chromium browser.
$browsers = @(
    @{ Key = "HKCU:\Software\Google\Chrome\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" },
    @{ Key = "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" },
    @{ Key = "HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" },
    @{ Key = "HKCU:\Software\Vivaldi\Vivaldi\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" },
    @{ Key = "HKCU:\Software\OperaSoftware\Opera\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx" }
)

foreach ($b in $browsers) {
    $key = $b.Key
    # Create the key path (will fail silently if it exists).
    $parent = Split-Path $key -Parent
    if (-not (Test-Path $parent)) {
        New-Item -Path $parent -Force | Out-Null
    }
    if (-not (Test-Path $key)) {
        New-Item -Path $key -Force | Out-Null
    }
    Set-ItemProperty -Path $key -Name "(default)" -Value $manifestPath -Type String
    Write-Host "Registered: $key"
}

# Firefox uses a different location: a JSON file at
# %APPDATA%\mozilla\native-messaging-hosts\io.github.mohammadibrahimdev.zavyx.json
$ffPath = Join-Path $env:APPDATA "mozilla\native-messaging-hosts"
if (-not (Test-Path $ffPath)) {
    New-Item -ItemType Directory -Path $ffPath -Force | Out-Null
}
$ffManifest = Join-Path $ffPath "io.github.mohammadibrahimdev.zavyx.json"
Copy-Item $manifestPath $ffManifest -Force
Write-Host "Firefox manifest: $ffManifest"

# Firefox manifest needs `allowed_extensions` instead of `allowed_origins`.
$ffManifestObj = Get-Content $ffManifest | ConvertFrom-Json
$ffManifestObj.PSObject.Properties.Remove("allowed_origins")
$ffManifestObj | Add-Member -NotePropertyName allowed_extensions -NotePropertyValue @("zavyx@mohammadibrahimdev.github.io")
[System.IO.File]::WriteAllText($ffManifest, ($ffManifestObj | ConvertTo-Json -Depth 4), $utf8NoBom)

# On Windows, Firefox locates hosts only via the registry, not the folder above.
$ffKey = "HKCU:\Software\Mozilla\NativeMessagingHosts\io.github.mohammadibrahimdev.zavyx"
if (-not (Test-Path $ffKey)) {
    New-Item -Path $ffKey -Force | Out-Null
}
Set-ItemProperty -Path $ffKey -Name "(default)" -Value $ffManifest -Type String
Write-Host "Registered: $ffKey"

Write-Host ""
Write-Host "Done. Restart your browser(s) for the changes to take effect."
