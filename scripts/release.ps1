# Local release bundle (Windows): builds the release binary, checksums
# it, and smoke-tests the artifact. The tag-driven CI workflow
# (.github/workflows/release.yml) performs the same assembly on both
# platforms and publishes the release once the repository is hosted.
#
# Usage:
#   powershell -File scripts/release.ps1
#
# The published flow is docs/release/RELEASE-CHECKLIST.md.

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

# 1. The tree must be clean — a release never carries uncommitted work.
$status = git status --porcelain
if ($status) {
    Write-Error "the working tree is not clean - commit or discard changes before releasing"
    exit 1
}

# 2. Build the release binary.
cargo build --release
if ($LASTEXITCODE -ne 0) { Write-Error "cargo build --release failed"; exit 1 }

# 3. Assemble dist/ (gitignored) with the checksum file.
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$bin = Join-Path $root "target\release\vaultline.exe"
$version = (& $bin version --json | ConvertFrom-Json).version
$asset = "vaultline-$version-x86_64-pc-windows-msvc.exe"
Copy-Item $bin (Join-Path $dist $asset) -Force
$hash = (Get-FileHash -Algorithm SHA256 (Join-Path $dist $asset)).Hash.ToLower()
"$hash  $asset" | Out-File -Encoding ascii (Join-Path $dist "SHA256SUMS")
Write-Host "bundle: $dist\$asset"
Write-Host "        $dist\SHA256SUMS"

# 4. Smoke: the artifact reports its version and writes a valid template.
$smoke = Join-Path $env:TEMP "vaultline-release-smoke"
Remove-Item -Recurse -Force $smoke -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $smoke | Out-Null
& (Join-Path $dist $asset) version
if ($LASTEXITCODE -ne 0) { Write-Error "smoke: version failed"; exit 1 }
& (Join-Path $dist $asset) init --config (Join-Path $smoke "vaultline.toml") --name smoke-app | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Error "smoke: init failed"; exit 1 }
& (Join-Path $dist $asset) validate --config (Join-Path $smoke "vaultline.toml")
if ($LASTEXITCODE -ne 0) { Write-Error "smoke: validate failed"; exit 1 }
Write-Host "release bundle assembled and smoke-tested (dist/ is gitignored)"
