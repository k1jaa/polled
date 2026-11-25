# Polled Release Packaging & Verification Checklist
# Usage: .\scripts\release-checklist.ps1 [-Version "0.12.0"] [-SkipBuild]
param(
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $RepoRoot

try {
    if ([string]::IsNullOrWhiteSpace($Version)) {
        $CargoToml = Get-Content (Join-Path $RepoRoot "Cargo.toml") -Raw
        if ($CargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
            $Version = $Matches[1]
        } else {
            throw "Could not detect workspace version from Cargo.toml"
        }
    }

    Write-Host "=== Polled v$Version Release Verification ===" -ForegroundColor Cyan

    Write-Host "[1/6] Checking formatting..."
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt check failed" }

    Write-Host "[2/6] Running clippy (-D warnings)..."
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

    Write-Host "[3/6] Running full workspace test suite..."
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

    if (-not $SkipBuild) {
        Write-Host "[4/6] Building release binaries (polled-cli + polled-gui)..."
        cargo build --release --workspace
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
    } else {
        Write-Host "[4/6] Skipping release build (-SkipBuild)."
    }

    $CliExe = Join-Path $RepoRoot "target\release\polled.exe"
    $GuiExe = Join-Path $RepoRoot "target\release\polled-gui.exe"
    if (-not (Test-Path $CliExe) -or -not (Test-Path $GuiExe)) {
        throw "Release binaries missing in target\release (ensure polled-gui.exe is closed before building)."
    }

    Write-Host "[5/6] Staging portable release ZIP..."
    $DistDir = Join-Path $RepoRoot "dist"
    $StageDir = Join-Path $DistDir "polled-v$Version-windows-x86_64"
    $ZipPath = Join-Path $DistDir "polled-v$Version-windows-x86_64.zip"
    if (Test-Path $StageDir) { Remove-Item $StageDir -Recurse -Force }
    if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
    New-Item -ItemType Directory -Path $StageDir -Force | Out-Null

    Copy-Item $GuiExe $StageDir
    Copy-Item $CliExe $StageDir
    Copy-Item (Join-Path $RepoRoot "README.md") $StageDir
    Copy-Item (Join-Path $RepoRoot "LICENSE") $StageDir
    Copy-Item (Join-Path $RepoRoot "CHANGELOG.md") $StageDir

    Compress-Archive -Path (Join-Path $StageDir "*") -DestinationPath $ZipPath -CompressionLevel Optimal

    $ZipHash = (Get-FileHash $ZipPath -Algorithm SHA256).Hash.ToUpperInvariant()
    $CliHash = (Get-FileHash $CliExe -Algorithm SHA256).Hash.ToUpperInvariant()
    $GuiHash = (Get-FileHash $GuiExe -Algorithm SHA256).Hash.ToUpperInvariant()

    $ChecksumsPath = Join-Path $DistDir "SHA256SUMS-v$Version.txt"
    @(
        "$ZipHash  polled-v$Version-windows-x86_64.zip"
        "$GuiHash  polled-gui.exe"
        "$CliHash  polled.exe"
    ) | Set-Content -Path $ChecksumsPath -Encoding ASCII

    Write-Host "[6/6] Updating WinGet installer manifest SHA-256 preview..."
    Write-Host ""
    Write-Host "Release archive ready:" -ForegroundColor Green
    Write-Host "  ZIP:       $ZipPath"
    Write-Host "  SHA-256:   $ZipHash"
    Write-Host "  Checksums: $ChecksumsPath"
    Write-Host ""
    Write-Host "Next steps to publish:" -ForegroundColor Yellow
    Write-Host "  1. git tag -a v$Version -m 'Release v$Version' && git push origin main --tags"
    Write-Host "  2. Upload ZIP + SHA256SUMS to https://github.com/k1jaa/polled/releases/new?tag=v$Version"
    Write-Host "  3. Set InstallerSha256 in packaging/winget/k1jaa.Polled.installer.yaml to:"
    Write-Host "     $ZipHash"
    Write-Host "  4. Validate with: winget validate --manifest packaging/winget"
} finally {
    Pop-Location
}
