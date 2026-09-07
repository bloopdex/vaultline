# Vaultline installer (Windows x86_64).
#
# Downloads the release binary for the declared version, verifies its
# SHA-256 against the published checksum file, and installs it to
# $env:LOCALAPPDATA\Programs\vaultline\vaultline.exe.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/install.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/install.ps1 -Version 0.8.0
#
# The release base is the repository's published release downloads;
# -ReleaseBase overrides it (mirrors, self-hosted proxies).
param(
    [string]$Version = "0.8.0",
    [string]$ReleaseBase = "https://github.com/bloopdex/vaultline/releases/download"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$asset = "vaultline-$Version-x86_64-pc-windows-msvc.exe"
$urlBase = "$ReleaseBase/v$Version"
$temp = Join-Path $env:TEMP "vaultline-install"
New-Item -ItemType Directory -Force $temp | Out-Null

Write-Host "downloading $urlBase/SHA256SUMS"
Invoke-WebRequest -Uri "$urlBase/SHA256SUMS" -OutFile (Join-Path $temp "SHA256SUMS")
Write-Host "downloading $urlBase/$asset"
Invoke-WebRequest -Uri "$urlBase/$asset" -OutFile (Join-Path $temp $asset)

# Verify: the expected checksum is the entry naming this asset.
$expected = (Select-String -Path (Join-Path $temp "SHA256SUMS") -Pattern $asset -SimpleMatch).Line.Split(" ")[0]
$actual = (Get-FileHash -Algorithm SHA256 (Join-Path $temp $asset)).Hash.ToLower()
if ($actual -ne $expected) {
    Write-Error "checksum mismatch for ${asset}: expected $expected, got $actual - the download is not installed"
    exit 1
}
Write-Host "checksum verified: $actual"

$targetDir = Join-Path $env:LOCALAPPDATA "Programs\vaultline"
New-Item -ItemType Directory -Force $targetDir | Out-Null
Copy-Item (Join-Path $temp $asset) (Join-Path $targetDir "vaultline.exe") -Force
Write-Host "installed $targetDir\vaultline.exe"
& (Join-Path $targetDir "vaultline.exe") version
Write-Host ""
Write-Host "add $targetDir to your PATH to use vaultline from any shell"
