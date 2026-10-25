# Publish a new Zavyx version in one step.
#
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 0.2.0
#
# Sets the version in Cargo.toml and the browser extension, commits, tags
# v<version> and pushes. GitHub Actions then builds the installer and creates
# the Release page automatically (see .github\workflows\release.yml).

param([Parameter(Mandatory = $true)][string]$Version)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3" }
if (git status --porcelain) { throw "Commit or stash your changes first." }

# Cargo.toml (workspace version) and the extension manifest.
$cargo = Get-Content Cargo.toml -Raw
$cargo = [regex]::Replace($cargo, '(?m)^version = "[^"]+"', "version = `"$Version`"", 1)
[System.IO.File]::WriteAllText("$root\Cargo.toml", $cargo)
$manifest = Get-Content extension\manifest.json -Raw
$manifest = [regex]::Replace($manifest, '"version": "[^"]+"', "`"version`": `"$Version`"", 1)
[System.IO.File]::WriteAllText("$root\extension\manifest.json", $manifest)

cargo update --workspace --quiet
cargo test --workspace --quiet
if ($LASTEXITCODE -ne 0) { throw "Tests failed; nothing was released." }

git add -A
git commit -m "Release v$Version"
git tag "v$Version"
git push origin HEAD "v$Version"
Write-Host ""
Write-Host "Pushed v$Version. The release will appear in a few minutes at:"
Write-Host "https://github.com/mohammadibrahimdev/zavyx/releases"
